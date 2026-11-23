//! One way to save on every console page (issue #958): a bar at the foot of each form that says
//! whether the form holds unsaved changes, is being saved, was saved, or was refused and why, with
//! "Discard changes" and "Save" always in the same place. It sticks to the bottom of the page while
//! its form is in view, so a long form never hides whether its changes are kept.

use leptos::prelude::*;

use crate::admin::admin_error_i18n_key;
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::state::{AppState, use_app_state};

/// What the bar says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStatus {
  /// The form shows what is stored; nothing was saved while the page was open.
  Clean,
  Unsaved,
  Saving,
  Saved,
  /// The last save was refused and the form still holds its changes.
  Failed,
}

/// Where the last save of one form stands. The bar combines it with whether the form differs from
/// what is stored ([`status_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SavePhase {
  Idle,
  Saving,
  Saved,
  Failed,
}

/// The save state of one form: started with [`SaveTracker::begin`] right after the form hands its
/// change to the backend, settled by the backend's answer (a counter of finished changes and the
/// console's error), or by [`SaveTracker::settled`] when the stored values the form shows change.
#[derive(Clone, Copy)]
pub struct SaveTracker {
  state: AppState,
  phase: RwSignal<SavePhase>,
  writes_before: StoredValue<u64>,
  error_before: StoredValue<Option<String>>,
}

pub fn use_save_tracker() -> SaveTracker {
  let state = use_app_state();
  let phase = RwSignal::new(SavePhase::Idle);
  let writes_before = StoredValue::new(0);
  let error_before = StoredValue::new(None::<String>);
  let writes = Memo::new(move |_| state.read(|backend| backend.admin_writes_done()));
  let error = Memo::new(move |_| state.read(|backend| backend.admin_error()));
  Effect::new(move |_| {
    let writes = writes.get();
    let maybe_error = error.get();
    if phase.get_untracked() != SavePhase::Saving {
      return;
    }
    let is_new_refusal = maybe_error.is_some() && maybe_error != error_before.get_value();
    if is_new_refusal {
      phase.set(SavePhase::Failed);
    } else if writes > writes_before.get_value() {
      phase.set(SavePhase::Saved);
    }
  });
  SaveTracker {
    state,
    phase,
    writes_before,
    error_before,
  }
}

impl SaveTracker {
  /// Called right after the change went to the backend, so the error it cleared isn't taken for a
  /// refusal of this one.
  pub fn begin(&self) {
    let writes = self.state.try_read(|backend| backend.admin_writes_done());
    self.writes_before.set_value(writes.unwrap_or_default());
    let maybe_error = self.state.try_read(|backend| backend.admin_error());
    self.error_before.set_value(maybe_error.flatten());
    self.phase.set(SavePhase::Saving);
  }

  /// The stored values changed: a save under way has arrived.
  pub fn settled(&self) {
    if self.phase.get_untracked() == SavePhase::Saving {
      self.phase.set(SavePhase::Saved);
    }
  }

  /// The form was discarded or saved by other means: no outcome to show any more.
  pub fn clear(&self) {
    self.phase.set(SavePhase::Idle);
  }

  pub fn is_saving(&self) -> bool {
    self.phase.get() == SavePhase::Saving
  }

  pub fn status(&self, is_dirty: bool) -> SaveStatus {
    status_of(self.phase.get(), is_dirty)
  }
}

/// A save under way wins; a refusal shows while the refused changes are still in the form; then
/// unsaved changes; "saved" stays until the next edit.
fn status_of(phase: SavePhase, is_dirty: bool) -> SaveStatus {
  match (phase, is_dirty) {
    (SavePhase::Saving, _) => SaveStatus::Saving,
    (SavePhase::Failed, true) => SaveStatus::Failed,
    (_, true) => SaveStatus::Unsaved,
    (SavePhase::Saved, false) => SaveStatus::Saved,
    (SavePhase::Idle | SavePhase::Failed, false) => SaveStatus::Clean,
  }
}

/// The foot of a form: its save status, any further actions of the form (`children`, e.g. "Reset to
/// configuration file"), "Discard changes" and the submit button. The form's `on:submit` saves.
#[component]
pub fn SaveBar(
  tracker: SaveTracker,
  /// The form differs from what is stored.
  #[prop(into)]
  is_dirty: Signal<bool>,
  #[prop(into)] on_discard: Callback<()>,
  /// Saving isn't possible for a reason the page shows (a read-only policy).
  #[prop(into, optional)]
  is_locked: Signal<bool>,
  /// The submit button's text; "Save" by default.
  #[prop(into, optional)]
  save_label: Option<Signal<String>>,
  /// What the bar says while saving and once saved, where the page has more to say ("Signing and
  /// publishing…", "Saved as policy version 7").
  #[prop(into, optional)]
  saving_text: Option<Signal<String>>,
  #[prop(into, optional)] saved_text: Option<Signal<String>>,
  #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let status = Memo::new(move |_| tracker.status(is_dirty.get()));
  let maybe_error = Memo::new(move |_| state.read(|backend| backend.admin_error()));
  let text = move || match status.get() {
    SaveStatus::Clean => String::new(),
    SaveStatus::Unsaved => i18n.t("admin-save-unsaved"),
    SaveStatus::Saving => saving_text.map_or_else(|| i18n.t("admin-save-saving"), |text| text.get()),
    SaveStatus::Saved => saved_text.map_or_else(|| i18n.t("admin-save-saved"), |text| text.get()),
    SaveStatus::Failed => i18n.t_with(
      "admin-save-failed",
      &[("reason", &refusal_text(i18n, maybe_error.get()))],
    ),
  };
  let icon = move || match status.get() {
    SaveStatus::Clean => None,
    SaveStatus::Unsaved => Some(view! { <span class="save-dot" aria-hidden="true"></span> }.into_any()),
    SaveStatus::Saving => Some(view! { <span class="spinner" aria-hidden="true"></span> }.into_any()),
    SaveStatus::Saved => Some(view! { <Icon name=IconName::Check /> }.into_any()),
    SaveStatus::Failed => Some(view! { <Icon name=IconName::Alert /> }.into_any()),
  };
  view! {
    <div
      class="save-bar"
      class:is-dirty=move || matches!(status.get(), SaveStatus::Unsaved | SaveStatus::Failed | SaveStatus::Saving)
      class:is-failed=move || status.get() == SaveStatus::Failed
      class:is-saved=move || status.get() == SaveStatus::Saved
    >
      // A refusal is an alert next to the submit button (forms.md); the rest is polite status.
      <p class="save-status" role=move || if status.get() == SaveStatus::Failed { "alert" } else { "status" }>
        {icon}
        <span>{text}</span>
      </p>
      <div class="save-actions">
        {children.map(|children| children())}
        <button
          type="button"
          class="btn"
          prop:disabled=move || !is_dirty.get() || tracker.is_saving()
          on:click=move |_| {
            tracker.clear();
            on_discard.run(());
          }
        >
          {move || i18n.t("admin-save-discard")}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          prop:disabled=move || !is_dirty.get() || is_locked.get() || tracker.is_saving()
          aria-busy=move || tracker.is_saving().then_some("true")
        >
          {move || save_label.map_or_else(|| i18n.t("admin-save"), |label| label.get())}
        </button>
      </div>
    </div>
  }
}

/// The server's or the core's refusal in words, the same as the console's banner.
pub fn refusal_text(i18n: I18n, maybe_detail: Option<String>) -> String {
  let Some(detail) = maybe_detail else {
    return i18n.t("admin-save-failed-unknown");
  };
  match admin_error_i18n_key(&detail) {
    Some(key) => i18n.t(key),
    None => i18n.t_with("admin-error", &[("detail", &detail)]),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_save_under_way_shows_even_after_further_edits() {
    assert_eq!(status_of(SavePhase::Saving, true), SaveStatus::Saving);
    assert_eq!(status_of(SavePhase::Saving, false), SaveStatus::Saving);
  }

  #[test]
  fn a_refusal_shows_while_its_changes_are_still_in_the_form() {
    assert_eq!(status_of(SavePhase::Failed, true), SaveStatus::Failed);
    assert_eq!(status_of(SavePhase::Failed, false), SaveStatus::Clean);
  }

  #[test]
  fn saved_stays_until_the_next_edit() {
    assert_eq!(status_of(SavePhase::Saved, false), SaveStatus::Saved);
    assert_eq!(status_of(SavePhase::Saved, true), SaveStatus::Unsaved);
  }

  #[test]
  fn an_untouched_form_says_nothing() {
    assert_eq!(status_of(SavePhase::Idle, false), SaveStatus::Clean);
    assert_eq!(status_of(SavePhase::Idle, true), SaveStatus::Unsaved);
  }
}
