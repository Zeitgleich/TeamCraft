//! Live captions in the call screen (#215, e2ee.md §8.6).
//!
//! - **Button:** "Captions" in the control bar, only where the app captions calls (the desktop
//!   app: the snapshot carries the speech model's state) and this computer can run the model.
//!   Off at the start of every call.
//! - **First use:** the speech model isn't shipped with the app. The first press asks before it
//!   downloads it, saying how large it is and that the call's audio is recognized on this computer
//!   and never leaves it. The download's progress shows in the captions panel.
//! - **Panel:** over the lower edge of the stage, the latest lines with the speaker's name; a
//!   sentence still going on is shown lighter and replaced as it grows. Its header says the
//!   captions are only on this device, and picks the language (remembered in the device's call
//!   settings).
//! - **Screen readers:** finished sentences are announced politely, one at a time, the parts so far
//!   are not (they would repeat every second and a half).

use leptos::prelude::*;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::calls::{CallCaptions, CaptionLanguage, CaptionLine, CaptionModelState};

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::{AppState, use_app_state};

/// Lines the panel shows: enough to follow a quick exchange, few enough not to cover the stage.
const SHOWN_LINES: usize = 3;

/// The captions button's state for the active call, and the first-use question.
#[derive(Clone, Copy)]
pub struct CaptionsControl {
  /// The button shows: the app captions calls and this computer can run the model.
  pub is_offered: Memo<bool>,
  pub is_on: Memo<bool>,
  /// The first-use question about the model's download is open.
  pub is_asking: RwSignal<bool>,
  model: Memo<Option<CaptionModelState>>,
  state: AppState,
}

/// What a press of the captions button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
  TurnOff,
  TurnOn,
  /// The model isn't downloaded: ask first.
  Ask,
  /// The model is downloading for a start already asked for: nothing more to do.
  Wait,
}

/// The captions control for the active call.
pub fn use_captions_control() -> CaptionsControl {
  let state = use_app_state();
  let model = Memo::new(move |_| state.read(|backend| backend.caption_model()));
  let is_offered = Memo::new(move |_| {
    model
      .get()
      .is_some_and(|model| !matches!(model, CaptionModelState::Unsupported | CaptionModelState::Unknown))
  });
  let is_on = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .is_some_and(|active| active.maybe_captions.is_some())
    })
  });
  CaptionsControl {
    is_offered,
    is_on,
    is_asking: RwSignal::new(false),
    model,
    state,
  }
}

impl CaptionsControl {
  /// Turns captions on (asking about the download first where the model is missing) or off.
  pub fn toggle(&self) {
    match press(self.is_on.get_untracked(), self.model.get_untracked()) {
      Press::TurnOff => self.set(false),
      Press::TurnOn => self.set(true),
      Press::Ask => self.is_asking.set(true),
      Press::Wait => {},
    }
  }

  /// The user agreed to the download: captions start once the model is there.
  fn confirm_download(&self) {
    self.is_asking.set(false);
    self.set(true);
  }

  fn set_language(&self, language: CaptionLanguage) {
    self
      .state
      .settings
      .prefs
      .update(|prefs| prefs.calls.caption_language = language);
    if self.is_on.get_untracked() {
      self.set(true);
    }
  }

  fn set(&self, is_on: bool) {
    let language = self
      .state
      .settings
      .prefs
      .with_untracked(|prefs| prefs.calls.caption_language);
    self
      .state
      .write(|backend| backend.set_call_captions(is_on, language));
  }
}

/// The first-use question and, while captions are on, their panel over the stage.
#[component]
pub fn CaptionsPanel(control: CaptionsControl) -> impl IntoView {
  let state = control.state;
  let captions =
    Memo::new(move |_| state.read(|backend| backend.active_call().and_then(|active| active.maybe_captions)));
  view! {
    <Show when=move || control.is_asking.get() fallback=|| ()>
      <CaptionsQuestion control=control />
    </Show>
    {move || captions.get().map(|captions| view! { <CaptionsShown captions=captions control=control /> })}
  }
}

#[component]
fn CaptionsQuestion(control: CaptionsControl) -> impl IntoView {
  let i18n = use_i18n();
  let size = move || match control.model.get() {
    Some(CaptionModelState::Missing { size_bytes } | CaptionModelState::Ready { size_bytes }) => {
      format_bytes(size_bytes)
    },
    _ => String::new(),
  };
  view! {
    <div class="call-captions-question" role="alertdialog" aria-labelledby="captions-question-title" aria-describedby="captions-question-text">
      <h2 id="captions-question-title">{move || i18n.t("call-captions-ask-title")}</h2>
      <p id="captions-question-text">{move || i18n.t_with("call-captions-ask-text", &[("size", &size())])}</p>
      <div class="call-captions-question-actions">
        <button type="button" class="btn btn-small" on:click=move |_| control.is_asking.set(false)>
          {move || i18n.t("call-captions-ask-cancel")}
        </button>
        <button type="button" class="btn btn-small btn-primary" on:click=move |_| control.confirm_download()>
          {move || i18n.t("call-captions-ask-download")}
        </button>
      </div>
    </div>
  }
}

#[component]
fn CaptionsShown(captions: CallCaptions, control: CaptionsControl) -> impl IntoView {
  let i18n = use_i18n();
  let state = control.state;
  let name = move |line: &CaptionLine| {
    state
      .read(|backend| backend.user(line.user))
      .map(|user| user.display_name)
      .filter(|name| !name.is_empty())
      .unwrap_or_else(|| i18n.t("call-captions-someone"))
  };
  let (is_starting, has_lines) = (captions.is_starting, !captions.lines.is_empty());
  // The download's progress changes without the captions: read reactively.
  let status = move || match (control.model.get(), is_starting) {
    (
      Some(CaptionModelState::Downloading {
        received_bytes,
        size_bytes,
      }),
      _,
    ) => Some(i18n.t_with(
      "call-captions-downloading",
      &[("percent", &percent(received_bytes, size_bytes).to_string())],
    )),
    (_, true) => Some(i18n.t("call-captions-starting")),
    // Nobody said anything yet.
    _ if !has_lines => Some(i18n.t("call-captions-listening")),
    _ => None,
  };
  let shown = shown_lines(&captions.lines);
  let maybe_announced = captions
    .lines
    .iter()
    .rev()
    .find(|line| line.is_final)
    .map(|line| format!("{}: {}", name(line), line.text));
  let language = captions.language;
  view! {
    <section class="call-captions" aria-label=move || i18n.t("call-captions-label")>
      <header class="call-captions-header">
        <Icon name=IconName::Captions />
        <span class="call-captions-scope">{move || i18n.t("call-captions-only-here")}</span>
        <label class="visually-hidden" for="call-captions-language">{move || i18n.t("call-captions-language")}</label>
        <select
          id="call-captions-language"
          class="call-captions-language"
          on:change=move |event| control.set_language(CaptionLanguage::from_str_or_auto(&event_target_value(&event)))
        >
          {CaptionLanguage::ALL
            .into_iter()
            .map(|option| {
              view! {
                <option value=option.as_str() selected=option == language>
                  {move || i18n.t(language_key(option))}
                </option>
              }
            })
            .collect_view()}
        </select>
      </header>
      {move || status().map(|status| view! { <p class="call-captions-status">{status}</p> })}
      <div class="call-captions-lines" aria-hidden="true">
        {shown
          .into_iter()
          .map(|line| {
            let speaker = name(&line);
            view! {
              <p class="call-captions-line" class:is-partial=!line.is_final>
                <span class="call-captions-speaker">{speaker}</span>
                " "
                <span>{line.text}</span>
              </p>
            }
          })
          .collect_view()}
      </div>
      <p class="visually-hidden" aria-live="polite">{maybe_announced}</p>
    </section>
  }
}

fn press(is_on: bool, maybe_model: Option<CaptionModelState>) -> Press {
  match (is_on, maybe_model) {
    (true, _) => Press::TurnOff,
    (false, Some(CaptionModelState::Ready { .. })) => Press::TurnOn,
    (false, Some(CaptionModelState::Downloading { .. })) => Press::Wait,
    (false, _) => Press::Ask,
  }
}

/// The last [`SHOWN_LINES`] lines, oldest first.
fn shown_lines(lines: &[CaptionLine]) -> Vec<CaptionLine> {
  lines[lines.len().saturating_sub(SHOWN_LINES)..].to_vec()
}

fn percent(received_bytes: u64, size_bytes: u64) -> u64 {
  if size_bytes == 0 {
    return 0;
  }
  (received_bytes.saturating_mul(100) / size_bytes).min(100)
}

fn language_key(language: CaptionLanguage) -> &'static str {
  match language {
    CaptionLanguage::Auto => "call-captions-language-auto",
    CaptionLanguage::English => "call-captions-language-en",
    CaptionLanguage::German => "call-captions-language-de",
    CaptionLanguage::French => "call-captions-language-fr",
    CaptionLanguage::Spanish => "call-captions-language-es",
    CaptionLanguage::Italian => "call-captions-language-it",
    CaptionLanguage::Dutch => "call-captions-language-nl",
    CaptionLanguage::Polish => "call-captions-language-pl",
    CaptionLanguage::Portuguese => "call-captions-language-pt",
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake, UserId};

  use super::*;

  #[test]
  fn a_press_asks_before_the_download_and_waits_during_it() {
    let missing = Some(CaptionModelState::Missing { size_bytes: 10 });
    let downloading = Some(CaptionModelState::Downloading {
      received_bytes: 1,
      size_bytes: 10,
    });
    let ready = Some(CaptionModelState::Ready { size_bytes: 10 });
    assert_eq!(press(false, missing), Press::Ask);
    assert_eq!(press(false, downloading), Press::Wait);
    assert_eq!(press(false, ready), Press::TurnOn);
    assert_eq!(press(true, ready), Press::TurnOff);
    assert_eq!(press(true, downloading), Press::TurnOff);
  }

  #[test]
  fn the_panel_shows_the_latest_lines() {
    let lines: Vec<_> = (1..=5).map(line).collect();
    let shown = shown_lines(&lines);
    assert_eq!(shown.iter().map(|line| line.id).collect::<Vec<_>>(), [3, 4, 5]);
    assert_eq!(shown_lines(&lines[..1]).len(), 1);
    assert!(shown_lines(&[]).is_empty());
  }

  #[test]
  fn download_progress_is_a_bounded_percentage() {
    assert_eq!(percent(0, 0), 0);
    assert_eq!(percent(50, 200), 25);
    assert_eq!(percent(300, 200), 100);
  }

  #[test]
  fn every_language_has_a_name() {
    let keys: std::collections::BTreeSet<_> = CaptionLanguage::ALL.into_iter().map(language_key).collect();
    assert_eq!(keys.len(), CaptionLanguage::ALL.len());
  }

  fn line(id: u64) -> CaptionLine {
    CaptionLine {
      id,
      user: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      text: format!("line {id}"),
      is_final: true,
      at: 0,
    }
  }
}
