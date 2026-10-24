//! The owners of the signed admin roster in the security section: who they are, when each last
//! signed, and warnings before the workspace could end up without anyone who can administer it
//! (admin.md §2.3, issue #188).

use leptos::prelude::*;

use crate::admin::{OwnerRosterStatus, OwnerWarning};
use crate::clock::{format_date_time, now_ms};
use crate::components::admin_policy::{PolicyDraft, SelectRow};
use crate::i18n::{I18n, use_i18n};
use crate::state::use_app_state;

/// Choices for the recommended number of owners; the policy allows up to 10.
const RECOMMENDED_OWNERS: [u8; 4] = [1, 2, 3, 4];

/// Owners, their last signatures and the warnings, plus the recommended minimum (signed policy).
#[component]
pub fn OwnersPanel(policy: PolicyDraft) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let draft = policy.draft;
  let roster = Memo::new(move |_| state.read(|backend| backend.admin_owner_roster()));
  let minimum_options = Signal::derive(move || {
    RECOMMENDED_OWNERS
      .into_iter()
      .map(|count| {
        (
          count.to_string(),
          i18n.t_count("policy-owners-count", usize::from(count)),
        )
      })
      .collect()
  });

  view! {
    <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
      <legend>{move || i18n.t("policy-owners")}</legend>
      <p class="muted-text small">{move || i18n.t("policy-owners-hint")}</p>
      {move || match roster.get() {
        Some(status) => view! { <OwnerList status=status /> }.into_any(),
        None => view! { <p class="muted-text small">{i18n.t("policy-owners-unknown")}</p> }.into_any(),
      }}
      <SelectRow
        id="policy-owners-minimum"
        label=Signal::derive(move || i18n.t("policy-owners-minimum"))
        options=minimum_options
        selected=Signal::derive(move || draft.get().owners.recommended_minimum.to_string())
        on_select=move |value: String| {
          let count = value.parse::<u8>().unwrap_or(teamcraft_proto::DEFAULT_RECOMMENDED_OWNERS);
          policy.edit(|p| p.owners.recommended_minimum = count);
        }
      />
      <p class="muted-text small">{move || i18n.t("policy-owners-minimum-hint")}</p>
    </fieldset>
  }
}

/// The warnings first, then each owner with their last signature and the roster's expiry.
#[component]
fn OwnerList(status: OwnerRosterStatus) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let now = now_ms();
  let locale = i18n.locale().tag();
  let warnings = status
    .warnings(now)
    .into_iter()
    .map(|warning| view! { <p class="auth-alert" role="status">{warning_text(i18n, warning)}</p> })
    .collect_view();
  let owners = status
    .owners
    .iter()
    .map(|owner| {
      let name = state
        .read(|backend| backend.user(owner.user))
        .map(|user| user.display_name)
        .unwrap_or_else(|| owner.user.to_string());
      let signed = match owner.maybe_last_signed_at {
        Some(at) => i18n.t_with(
          "policy-owners-last-signed",
          &[("time", &format_date_time(at, locale))],
        ),
        None => i18n.t("policy-owners-never-signed"),
      };
      view! {
        <li>
          <strong translate="no">{name}</strong>
          " "
          <span class="muted-text small">{signed}</span>
        </li>
      }
    })
    .collect_view();
  let valid_until = status.maybe_expires_at.filter(|at| *at > now).map(|at| {
    let text = i18n.t_with(
      "policy-owners-valid-until",
      &[("time", &format_date_time(at, locale))],
    );
    view! { <p class="muted-text small">{text}</p> }
  });

  view! {
    {warnings}
    <ul class="owner-list">{owners}</ul>
    {valid_until}
  }
}

fn warning_text(i18n: I18n, warning: OwnerWarning) -> String {
  match warning {
    OwnerWarning::Expired => i18n.t("policy-owners-expired"),
    OwnerWarning::ExpiresSoon { at } => i18n.t_with(
      "policy-owners-expires-soon",
      &[("time", &format_date_time(at, i18n.locale().tag()))],
    ),
    OwnerWarning::TooFewOwners { count, minimum } => i18n.t_with(
      "policy-owners-too-few",
      &[("count", &count.to_string()), ("minimum", &minimum.to_string())],
    ),
  }
}
