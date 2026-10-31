//! The import page of the console (admin.md §3.8, e2ee.md §4.10): the signed `import` group of the
//! workspace policy, which names the account `teamcraft-import` signs in as and until when it may
//! import and join conversations. Only owners sign.

use leptos::prelude::*;
use teamcraft_proto::{Role, UserId};

use crate::clock::{format_local_datetime_input, parse_local_datetime};
use crate::components::admin_console::SectionHeader;
use crate::components::admin_policy::{PolicyBlockerNote, PolicyFooter, SelectRow, use_policy_draft};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Days an import may run when the owner turns it on.
const DEFAULT_IMPORT_DAYS: i64 = 90;
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

#[component]
pub fn ImportSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;
  // The import account is an admin of the identity provider (admin.md §3.8).
  let candidates = Memo::new(move |_| {
    let mut admins: Vec<(UserId, String)> = state
      .read(|backend| backend.admin_users())
      .into_iter()
      .filter(|entry| entry.is_active && matches!(entry.role, Role::Admin | Role::Owner))
      .map(|entry| (entry.user.id, entry.user.display_name))
      .collect();
    admins.sort_by(|a, b| a.1.cmp(&b.1));
    admins
  });
  let account_options = Signal::derive(move || {
    std::iter::once((String::new(), i18n.t("import-account-none")))
      .chain(
        candidates
          .get()
          .into_iter()
          .map(|(id, name)| (id.to_string(), name)),
      )
      .collect()
  });
  let until_value = Signal::derive(move || {
    draft
      .get()
      .import
      .maybe_joins_until_ms
      .map(|until| {
        format_local_datetime_input(until)
          .chars()
          .take(10)
          .collect::<String>()
      })
      .unwrap_or_default()
  });
  let is_open = Signal::derive(move || {
    draft
      .get()
      .import
      .maybe_joins_until_ms
      .is_some_and(|until| until > crate::clock::now_ms())
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-import-title"))
      subtitle=Signal::derive(move || i18n.t("admin-import-subtitle"))
    />
    <p class="auth-alert" role="note">{move || i18n.t("import-e2ee-warning")}</p>
    <PolicyBlockerNote policy=policy />
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        policy.save();
      }
    >
      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("admin-import-title")}</legend>
        <SelectRow
          id="import-account"
          label=Signal::derive(move || i18n.t("import-account"))
          options=account_options
          selected=Signal::derive(move || draft.get().import.maybe_importer.map(|id| id.to_string()).unwrap_or_default())
          on_select=move |value: String| {
            policy.edit(|p| {
              p.import.maybe_importer = value.parse().ok();
              if p.import.maybe_importer.is_some() && p.import.maybe_joins_until_ms.is_none() {
                p.import.maybe_joins_until_ms = Some(crate::clock::now_ms() + DEFAULT_IMPORT_DAYS * DAY_MS);
              }
            })
          }
        />
        <p class="muted-text small">{move || i18n.t("import-account-hint")}</p>
        <div class="field inline">
          <label for="import-until">{move || i18n.t("import-until")}</label>
          <input
            id="import-until"
            class="input"
            type="date"
            prop:value=move || until_value.get()
            prop:disabled=move || draft.get().import.maybe_importer.is_none()
            on:change=move |ev| {
              // Until the end of the chosen day.
              let maybe_until = parse_local_datetime(&format!("{}T23:59", event_target_value(&ev)));
              policy.edit(|p| p.import.maybe_joins_until_ms = maybe_until);
            }
          />
        </div>
        <p class="muted-text small" role="status">
          {move || if is_open.get() { i18n.t("import-open") } else { i18n.t("import-closed") }}
        </p>
        <button
          type="button"
          class="btn btn-small"
          prop:disabled=move || !is_open.get()
          on:click=move |_| policy.edit(|p| p.import.maybe_joins_until_ms = None)
        >
          {move || i18n.t("import-end")}
        </button>
      </fieldset>
      <PolicyFooter policy=policy />
    </form>
  }
}
