use leptos::prelude::*;
use teamcraft_proto::HistoryPolicy;

use crate::admin::{AdminSelfJoin, DmScope, SecurityPolicy};
use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const SESSION_DAYS: [u32; 4] = [1, 7, 30, 90];
const IDLE_MINUTES: [u32; 4] = [15, 60, 120, 480];

/// Security policy: the signed settings that every member client enforces itself.
#[component]
pub fn SecuritySection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let stored = Memo::new(move |_| state.read(|backend| backend.security_policy()));
  let draft = RwSignal::new(stored.get_untracked());
  let saved_version = RwSignal::new(None::<u32>);
  let is_dirty = move || draft.get() != stored.get();

  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    state.write(|backend| backend.set_security_policy(draft.get_untracked()));
    let saved = stored.get_untracked();
    saved_version.set(Some(saved.version));
    draft.set(saved);
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-security-title"))
      subtitle=Signal::derive(move || i18n.t("admin-security-subtitle"))
    />
    <form class="policy-form" on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t("policy-self-join")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-self-join-hint")}</p>
        {AdminSelfJoin::ALL
          .into_iter()
          .map(|option| {
            view! {
              <label class="radio">
                <input
                  type="radio"
                  name="admin_self_join"
                  value=option.slug()
                  prop:checked=move || draft.get().admin_self_join == option
                  on:change=move |_| draft.update(|policy| policy.admin_self_join = option)
                />
                <span>
                  <strong>{move || i18n.t(&format!("policy-self-join-{}", option.slug()))}</strong>
                  <span class="muted-text small">{move || i18n.t(&format!("policy-self-join-{}-hint", option.slug()))}</span>
                </span>
              </label>
            }
          })
          .collect_view()}
      </fieldset>

      <fieldset class="policy-group">
        <legend>{move || i18n.t("policy-recovery")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-recovery-hint")}</p>
        <label class="checkbox">
          <input
            type="checkbox"
            name="org_recovery_for_members"
            prop:checked=move || draft.get().org_recovery_for_members
            on:change=move |ev| {
              let value = event_target_checked(&ev);
              update_flag(draft, move |policy| policy.org_recovery_for_members = value);
            }
          />
          <span>{move || i18n.t("policy-recovery-members")}</span>
        </label>
        <label class="checkbox">
          <input
            type="checkbox"
            name="org_recovery_for_guests"
            prop:checked=move || draft.get().org_recovery_for_guests
            on:change=move |ev| {
              let value = event_target_checked(&ev);
              update_flag(draft, move |policy| policy.org_recovery_for_guests = value);
            }
          />
          <span>{move || i18n.t("policy-recovery-guests")}</span>
        </label>
        <div class="field inline">
          <label for="policy-approvals">{move || i18n.t("policy-recovery-approvals")}</label>
          <select
            id="policy-approvals"
            class="input"
            prop:disabled=move || !draft.get().org_recovery_for_members && !draft.get().org_recovery_for_guests
            on:change=move |ev| {
              let value = event_target_value(&ev).parse::<u8>().unwrap_or(2);
              update_flag(draft, move |policy| policy.recovery_approvals_required = value);
            }
          >
            {[2_u8, 3]
              .into_iter()
              .map(|count| {
                view! {
                  <option value=count.to_string() selected=move || draft.get().recovery_approvals_required == count>
                    {move || i18n.t_count("policy-admins", count as usize)}
                  </option>
                }
              })
              .collect_view()}
          </select>
        </div>
      </fieldset>

      <fieldset class="policy-group">
        <legend>{move || i18n.t("policy-signin")}</legend>
        <label class="checkbox">
          <input
            type="checkbox"
            name="requires_mfa"
            prop:checked=move || draft.get().requires_mfa
            on:change=move |ev| {
              let value = event_target_checked(&ev);
              update_flag(draft, move |policy| policy.requires_mfa = value);
            }
          />
          <span>{move || i18n.t("policy-mfa")}</span>
        </label>
        <label class="checkbox">
          <input
            type="checkbox"
            name="requires_mfa_for_guests"
            prop:checked=move || draft.get().requires_mfa_for_guests
            on:change=move |ev| {
              let value = event_target_checked(&ev);
              update_flag(draft, move |policy| policy.requires_mfa_for_guests = value);
            }
          />
          <span>{move || i18n.t("policy-mfa-guests")}</span>
        </label>
        <div class="field inline">
          <label for="policy-session">{move || i18n.t("policy-session")}</label>
          <select
            id="policy-session"
            class="input"
            on:change=move |ev| {
              let value = event_target_value(&ev).parse::<u32>().unwrap_or(30);
              update_flag(draft, move |policy| policy.session_days = value);
            }
          >
            {SESSION_DAYS
              .into_iter()
              .map(|days| {
                view! {
                  <option value=days.to_string() selected=move || draft.get().session_days == days>
                    {move || i18n.t_count("policy-days", days as usize)}
                  </option>
                }
              })
              .collect_view()}
          </select>
        </div>
        <div class="field inline">
          <label for="policy-idle">{move || i18n.t("policy-idle")}</label>
          <select
            id="policy-idle"
            class="input"
            on:change=move |ev| {
              let value = event_target_value(&ev).parse::<u32>().unwrap_or(120);
              update_flag(draft, move |policy| policy.idle_timeout_minutes = value);
            }
          >
            {IDLE_MINUTES
              .into_iter()
              .map(|minutes| {
                view! {
                  <option value=minutes.to_string() selected=move || draft.get().idle_timeout_minutes == minutes>
                    {move || i18n.t_count("policy-minutes", minutes as usize)}
                  </option>
                }
              })
              .collect_view()}
          </select>
        </div>
      </fieldset>

      <fieldset class="policy-group">
        <legend>{move || i18n.t("policy-sharing")}</legend>
        <div class="field">
          <label for="policy-domains">{move || i18n.t("policy-guest-domains")}</label>
          <input
            id="policy-domains"
            class="input"
            name="guest_email_domains"
            autocomplete="off"
            spellcheck="false"
            placeholder="example.com, partner.example"
            prop:value=move || draft.get().guest_email_domains
            on:input=move |ev| {
              let value = event_target_value(&ev);
              update_flag(draft, move |policy| policy.guest_email_domains = value.clone());
            }
          />
        </div>
        <div class="field inline">
          <label for="policy-history">{move || i18n.t("policy-history")}</label>
          <select
            id="policy-history"
            class="input"
            on:change=move |ev| {
              let value = if event_target_value(&ev) == "full" { HistoryPolicy::Full } else { HistoryPolicy::FromJoin };
              update_flag(draft, move |policy| policy.default_history = value.clone());
            }
          >
            <option value="from_join" selected=move || draft.get().default_history == HistoryPolicy::FromJoin>
              {move || i18n.t("admin-history-from_join")}
            </option>
            <option value="full" selected=move || draft.get().default_history == HistoryPolicy::Full>
              {move || i18n.t("admin-history-full")}
            </option>
          </select>
        </div>
        <div class="field inline">
          <label for="policy-dm">{move || i18n.t("policy-dm")}</label>
          <select
            id="policy-dm"
            class="input"
            on:change=move |ev| {
              if let Some(scope) = DmScope::from_slug(&event_target_value(&ev)) {
                update_flag(draft, move |policy| policy.dm_scope = scope);
              }
            }
          >
            {DmScope::ALL
              .into_iter()
              .map(|scope| {
                view! {
                  <option value=scope.slug() selected=move || draft.get().dm_scope == scope>
                    {move || i18n.t(&format!("policy-dm-{}", scope.slug()))}
                  </option>
                }
              })
              .collect_view()}
          </select>
        </div>
      </fieldset>

      <footer class="policy-footer">
        <p class="signing-note">
          <Icon name=IconName::Key />
          <span>{move || i18n.t_with("policy-signing", &[("version", &stored.get().version.to_string())])}</span>
        </p>
        <span class="status-line" role="status">
          {move || saved_version.get().map(|version| i18n.t_with("policy-saved", &[("version", &version.to_string())]))}
        </span>
        <button type="button" class="btn" disabled=move || !is_dirty() on:click=move |_| draft.set(stored.get_untracked())>
          {move || i18n.t("policy-reset")}
        </button>
        <button type="submit" class="btn btn-primary" disabled=move || !is_dirty()>
          {move || i18n.t("policy-save")}
        </button>
      </footer>
    </form>
  }
}

fn update_flag(draft: RwSignal<SecurityPolicy>, change: impl FnOnce(&mut SecurityPolicy)) {
  draft.update(change);
}
