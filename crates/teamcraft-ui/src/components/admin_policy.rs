//! Shared parts of the console forms that edit the signed workspace policy: the draft with its
//! save/reset flow, the footer, and small form rows. Several sections (security, people,
//! notifications) edit different parts of the same policy; each keeps its own draft.

use leptos::prelude::*;
use teamcraft_proto::WorkspacePolicy;

use crate::admin::PolicyBlocker;
use crate::components::admin_save::{SaveBar, SaveTracker, use_save_tracker};
use crate::components::icons::{Icon, IconName};
use crate::form_errors::error_id;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// The policy in force, an editable copy of it, and the state of a save under way.
#[derive(Clone, Copy)]
pub struct PolicyDraft {
  pub stored: Memo<(u64, WorkspacePolicy)>,
  pub draft: RwSignal<WorkspacePolicy>,
  pub blocker: Memo<Option<PolicyBlocker>>,
  /// A signed policy reaches the server a moment after "save"; until its version shows up the form
  /// keeps the edits instead of snapping back to the old values.
  pending_after: RwSignal<Option<u64>>,
  saved_version: RwSignal<Option<u64>>,
  tracker: SaveTracker,
}

/// A role name left empty means the usual one: no role at the provider has an empty name.
fn fill_in_role_names(policy: &mut WorkspacePolicy) {
  if let Some(signing) = &mut policy.maybe_role_signing {
    if signing.owner_role.trim().is_empty() {
      teamcraft_proto::DEFAULT_OWNER_ROLE.clone_into(&mut signing.owner_role);
    }
    if signing.admin_role.trim().is_empty() {
      teamcraft_proto::DEFAULT_ADMIN_ROLE.clone_into(&mut signing.admin_role);
    }
  }
}

pub fn use_policy_draft() -> PolicyDraft {
  let state = use_app_state();
  let stored = Memo::new(move |_| state.read(|backend| backend.workspace_policy()));
  let draft = RwSignal::new(stored.get_untracked().1);
  let blocker = Memo::new(move |_| state.read(|backend| backend.policy_edit_blocker()));
  let pending_after = RwSignal::new(None::<u64>);
  let saved_version = RwSignal::new(None::<u64>);
  let tracker = use_save_tracker();
  // Follow the server while the form is untouched; keep the edits otherwise, except right after
  // our own save arrives.
  Effect::new(move |previous: Option<(u64, WorkspacePolicy)>| {
    let current = stored.get();
    if let Some(previous) = previous {
      let untouched = draft.get_untracked() == previous.1;
      let our_save_arrived = pending_after
        .get_untracked()
        .is_some_and(|before| current.0 > before);
      if untouched || our_save_arrived {
        draft.set(current.1.clone());
      }
      if our_save_arrived {
        pending_after.set(None);
        saved_version.set(Some(current.0));
        tracker.settled();
      }
    }
    current
  });
  PolicyDraft {
    stored,
    draft,
    blocker,
    pending_after,
    saved_version,
    tracker,
  }
}

impl PolicyDraft {
  pub fn is_dirty(&self) -> bool {
    self.draft.get() != self.stored.get().1
  }

  pub fn is_locked(&self) -> bool {
    self.blocker.get().is_some()
  }

  pub fn reset(&self) {
    self.pending_after.set(None);
    self.draft.set(self.stored.get_untracked().1);
  }

  pub fn edit(&self, change: impl FnOnce(&mut WorkspacePolicy)) {
    self.draft.update(change);
  }

  /// Saves the draft. In a workspace without a signed roster the signer goes on the first one, alone;
  /// everybody else follows from their own provider tokens (admin.md §2.3.1).
  pub fn save(&self) {
    let state = use_app_state();
    let before = self.stored.get_untracked().0;
    self.draft.update(fill_in_role_names);
    state.write(|backend| backend.set_workspace_policy(self.draft.get_untracked()));
    self.tracker.begin();
    let saved = self.stored.get_untracked();
    if saved.0 == before {
      self.pending_after.set(Some(before));
    } else {
      self.saved_version.set(Some(saved.0));
      self.draft.set(saved.1);
      self.tracker.settled();
    }
  }
}

/// Explains why the form is read-only, if it is.
#[component]
pub fn PolicyBlockerNote(policy: PolicyDraft) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    {move || {
      policy.blocker.get().map(|blocker| {
        let key = match blocker {
          PolicyBlocker::NotOwner => "policy-blocked-not-owner",
          PolicyBlocker::NoSigningKeys => "policy-blocked-no-keys",
        };
        view! { <p class="auth-alert policy-blocked" role="status">{move || i18n.t(key)}</p> }
      })
    }}
  }
}

/// What saving a policy does, in plain words, and the page's save bar.
#[component]
pub fn PolicyFooter(policy: PolicyDraft) -> impl IntoView {
  let i18n = use_i18n();
  let is_dirty = Signal::derive(move || policy.is_dirty());
  view! {
    <p class="signing-note">
      <Icon name=IconName::Key />
      <span>{move || i18n.t_with("policy-signing", &[("version", &policy.stored.get().0.to_string())])}</span>
    </p>
    <SaveBar
      tracker=policy.tracker
      is_dirty=is_dirty
      on_discard=Callback::new(move |()| policy.reset())
      is_locked=Signal::derive(move || policy.is_locked())
      save_label=Signal::derive(move || i18n.t("policy-save"))
      saving_text=Signal::derive(move || i18n.t("policy-saving"))
      saved_text=Signal::derive(move || {
        policy
          .saved_version
          .get()
          .map_or_else(|| i18n.t("admin-save-saved"), |version| i18n.t_with("policy-saved", &[("version", &version.to_string())]))
      })
    />
  }
}

/// A checkbox with a label and an optional hint.
#[component]
pub fn CheckRow(
  #[prop(into)] label: Signal<String>,
  #[prop(into, optional)] hint: Option<Signal<String>>,
  #[prop(into)] checked: Signal<bool>,
  #[prop(into)] on_change: Callback<bool>,
  #[prop(into, optional)] disabled: Signal<bool>,
  #[prop(optional)] name: &'static str,
) -> impl IntoView {
  view! {
    <label class="checkbox">
      <input
        type="checkbox"
        name=name
        prop:checked=move || checked.get()
        prop:disabled=move || disabled.get()
        on:change=move |ev| on_change.run(event_target_checked(&ev))
      />
      <span>
        {move || label.get()}
        {hint.map(|hint| view! { <span class="muted-text small hint-line">{move || hint.get()}</span> })}
      </span>
    </label>
  }
}

/// A labelled select. Option values are strings; the caller maps them to and from its types.
#[component]
pub fn SelectRow(
  id: &'static str,
  #[prop(into)] label: Signal<String>,
  #[prop(into)] options: Signal<Vec<(String, String)>>,
  #[prop(into)] selected: Signal<String>,
  #[prop(into)] on_select: Callback<String>,
  #[prop(into, optional)] disabled: Signal<bool>,
  /// What the chosen option means for people, in plain words, below the row (issue #2360).
  #[prop(into, optional)]
  consequence: Option<Signal<String>>,
  /// What is wrong with the choice, shown below the row (forms.md).
  #[prop(into, optional)]
  error: Signal<Option<String>>,
) -> impl IntoView {
  let consequence_id = format!("{id}-consequence");
  let has_consequence = consequence.is_some();
  let described = {
    let consequence_id = consequence_id.clone();
    move || {
      let ids: Vec<String> = [
        has_consequence.then(|| consequence_id.clone()),
        error.with(Option::is_some).then(|| error_id(id)),
      ]
      .into_iter()
      .flatten()
      .collect();
      (!ids.is_empty()).then(|| ids.join(" "))
    }
  };
  view! {
    <div class="field inline">
      <label for=id>{move || label.get()}</label>
      <select
        id=id
        class="input"
        aria-invalid=move || error.with(Option::is_some).then_some("true")
        aria-describedby=described
        prop:disabled=move || disabled.get()
        prop:value=move || selected.get()
        on:change=move |ev| on_select.run(event_target_value(&ev))
      >
        {move || {
          let current = selected.get();
          options
            .get()
            .into_iter()
            .map(|(value, text)| {
              let is_selected = value == current;
              view! { <option value=value selected=is_selected>{text}</option> }
            })
            .collect_view()
        }}
      </select>
    </div>
    {consequence.map(|text| {
      view! { <p class="muted-text small policy-consequence" id=consequence_id aria-live="polite">{move || text.get()}</p> }
    })}
    <p class="field-error" id=error_id(id)>{move || error.get()}</p>
  }
}
