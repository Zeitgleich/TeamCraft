use leptos::prelude::*;
use teamcraft_proto::{AdminSelfJoin, GuestDmStart, HistoryPolicy};

use crate::components::admin_console::SectionHeader;
use crate::components::admin_owners::OwnersPanel;
use crate::components::admin_policy::{
  CheckRow, PolicyBlockerNote, PolicyFooter, SelectRow, use_policy_draft,
};
use crate::i18n::use_i18n;

/// Session lengths offered, in hours; `0` stands for "no limit".
const SESSION_HOURS: [u32; 5] = [24, 7 * 24, 30 * 24, 90 * 24, 0];
const IDLE_MINUTES: [u32; 5] = [15, 60, 120, 480, 0];
const PASSWORD_CHARS: [u32; 5] = [0, 10, 12, 14, 16];
const GUEST_EXPIRY_DAYS: [u32; 6] = [u32::MAX, 7, 30, 90, 180, 0];
const APPROVALS: [u8; 2] = [2, 3];

/// Security policy: the signed settings that the server and every member client enforce.
#[component]
pub fn SecuritySection() -> impl IntoView {
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;

  let session_options = Signal::derive(move || {
    SESSION_HOURS
      .into_iter()
      .map(|hours| {
        let label = match hours {
          0 => i18n.t("policy-no-limit"),
          hours => i18n.t_count("policy-days", (hours / 24) as usize),
        };
        (hours.to_string(), label)
      })
      .collect()
  });
  let idle_options = Signal::derive(move || {
    IDLE_MINUTES
      .into_iter()
      .map(|minutes| {
        let label = match minutes {
          0 => i18n.t("policy-no-limit"),
          minutes => i18n.t_count("policy-minutes", minutes as usize),
        };
        (minutes.to_string(), label)
      })
      .collect()
  });
  let password_options = Signal::derive(move || {
    PASSWORD_CHARS
      .into_iter()
      .map(|chars| {
        let label = match chars {
          0 => i18n.t("policy-server-default"),
          chars => i18n.t_count("policy-chars", chars as usize),
        };
        (chars.to_string(), label)
      })
      .collect()
  });
  let expiry_options = Signal::derive(move || {
    GUEST_EXPIRY_DAYS
      .into_iter()
      .map(|days| {
        let label = match days {
          u32::MAX => i18n.t("policy-server-default"),
          0 => i18n.t("policy-guest-never"),
          days => i18n.t_count("policy-days", days as usize),
        };
        (days.to_string(), label)
      })
      .collect()
  });
  let dm_options = Signal::derive(move || {
    [
      ("default", "policy-server-default"),
      ("nobody", "policy-guest-dm-nobody"),
      ("shared", "policy-guest-dm-shared"),
      ("anyone", "policy-dm-anyone"),
    ]
    .into_iter()
    .map(|(value, key)| (value.to_owned(), i18n.t(key)))
    .collect()
  });
  let history_options = Signal::derive(move || {
    [
      ("from_join", "admin-history-from_join"),
      ("full", "admin-history-full"),
    ]
    .into_iter()
    .map(|(value, key)| (value.to_owned(), i18n.t(key)))
    .collect()
  });
  let approval_options = Signal::derive(move || {
    APPROVALS
      .into_iter()
      .map(|count| (count.to_string(), i18n.t_count("policy-admins", count as usize)))
      .collect()
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-security-title"))
      subtitle=Signal::derive(move || i18n.t("admin-security-subtitle"))
    />
    <PolicyBlockerNote policy=policy />
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        policy.save();
      }
    >
      <OwnersPanel policy=policy />

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-self-join")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-self-join-hint")}</p>
        {[AdminSelfJoin::Visible, AdminSelfJoin::OwnerApproval, AdminSelfJoin::Disabled]
          .into_iter()
          .map(|option| {
            let slug = option.as_str().to_owned();
            let hint_key = format!("policy-self-join-{slug}-hint");
            let label_key = format!("policy-self-join-{slug}");
            let checked_option = option.clone();
            view! {
              <label class="radio">
                <input
                  type="radio"
                  name="admin_self_join"
                  value=slug
                  prop:checked=move || draft.get().admin_self_join == checked_option
                  on:change=move |_| policy.edit(|value| value.admin_self_join = option.clone())
                />
                <span>
                  <strong>{move || i18n.t(&label_key)}</strong>
                  <span class="muted-text small">{move || i18n.t(&hint_key)}</span>
                </span>
              </label>
            }
          })
          .collect_view()}
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-backup")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-backup-hint")}</p>
        <CheckRow
          name="recovery_passphrase_allowed"
          label=Signal::derive(move || i18n.t("policy-backup-passphrase"))
          hint=Signal::derive(move || i18n.t("policy-backup-passphrase-hint"))
          checked=Signal::derive(move || draft.get().recovery.is_passphrase_allowed)
          on_change=move |value| policy.edit(|p| p.recovery.is_passphrase_allowed = value)
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-recovery")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-recovery-hint")}</p>
        <CheckRow
          name="org_recovery_for_members"
          label=Signal::derive(move || i18n.t("policy-recovery-members"))
          checked=Signal::derive(move || draft.get().org_recovery.is_enabled_for_members)
          on_change=move |value| policy.edit(|p| p.org_recovery.is_enabled_for_members = value)
        />
        <CheckRow
          name="org_recovery_for_guests"
          label=Signal::derive(move || i18n.t("policy-recovery-guests"))
          checked=Signal::derive(move || draft.get().org_recovery.is_enabled_for_guests)
          on_change=move |value| policy.edit(|p| p.org_recovery.is_enabled_for_guests = value)
        />
        <SelectRow
          id="policy-approvals"
          label=Signal::derive(move || i18n.t("policy-recovery-approvals"))
          options=approval_options
          selected=Signal::derive(move || draft.get().org_recovery.admins_required.to_string())
          on_select=move |value: String| {
            let count = value.parse::<u8>().unwrap_or(2);
            policy.edit(|p| p.org_recovery.admins_required = count);
          }
          disabled=Signal::derive(move || {
            let current = draft.get();
            !current.org_recovery.is_enabled_for_members && !current.org_recovery.is_enabled_for_guests
          })
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-signin")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-signin-hint")}</p>
        <CheckRow
          name="requires_mfa"
          label=Signal::derive(move || i18n.t("policy-mfa"))
          hint=Signal::derive(move || i18n.t("policy-mfa-hint"))
          checked=Signal::derive(move || draft.get().session.requires_mfa)
          on_change=move |value| policy.edit(|p| p.session.requires_mfa = value)
        />
        <SelectRow
          id="policy-session"
          label=Signal::derive(move || i18n.t("policy-session"))
          options=session_options
          selected=Signal::derive(move || draft.get().session.maybe_max_age_hours.unwrap_or(0).to_string())
          on_select=move |value: String| {
            let hours = value.parse::<u32>().unwrap_or(0);
            policy.edit(|p| p.session.maybe_max_age_hours = (hours > 0).then_some(hours));
          }
        />
        <SelectRow
          id="policy-idle"
          label=Signal::derive(move || i18n.t("policy-idle"))
          options=idle_options
          selected=Signal::derive(move || draft.get().session.maybe_idle_timeout_minutes.unwrap_or(0).to_string())
          on_select=move |value: String| {
            let minutes = value.parse::<u32>().unwrap_or(0);
            policy.edit(|p| p.session.maybe_idle_timeout_minutes = (minutes > 0).then_some(minutes));
          }
        />
        <SelectRow
          id="policy-password"
          label=Signal::derive(move || i18n.t("policy-password"))
          options=password_options
          selected=Signal::derive(move || draft.get().passwords.maybe_min_chars.unwrap_or(0).to_string())
          on_select=move |value: String| {
            let chars = value.parse::<u32>().unwrap_or(0);
            policy.edit(|p| p.passwords.maybe_min_chars = (chars > 0).then_some(chars));
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-password-hint")}</p>
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-guests")}</legend>
        <div class="field">
          <label for="policy-domains">{move || i18n.t("policy-guest-domains")}</label>
          <input
            id="policy-domains"
            class="input"
            name="guest_email_domains"
            autocomplete="off"
            spellcheck="false"
            placeholder="example.com, partner.example"
            prop:value=move || draft.get().guests.allowed_email_domains.join(", ")
            on:input=move |ev| {
              let domains = parse_domains(&event_target_value(&ev));
              policy.edit(|p| p.guests.allowed_email_domains = domains);
            }
          />
          <p class="muted-text small">{move || i18n.t("policy-guest-domains-hint")}</p>
        </div>
        <SelectRow
          id="policy-dm"
          label=Signal::derive(move || i18n.t("policy-guest-dm"))
          options=dm_options
          selected=Signal::derive(move || {
            let guests = draft.get().guests;
            let start = guests
              .maybe_dm_start
              .or_else(|| guests.maybe_dm_only_with_shared_conversations.map(GuestDmStart::from_shared_only_flag));
            match start {
              None => "default".to_owned(),
              Some(start) => start.to_string(),
            }
          })
          on_select=move |value: String| {
            let start = (value != "default").then(|| GuestDmStart::from(value));
            policy.edit(|p| p.guests.set_dm_start(start));
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-guest-dm-hint")}</p>
        <SelectRow
          id="policy-guest-expiry"
          label=Signal::derive(move || i18n.t("policy-guest-expiry"))
          options=expiry_options
          selected=Signal::derive(move || draft.get().guests.maybe_default_expiry_days.unwrap_or(u32::MAX).to_string())
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(u32::MAX);
            policy.edit(|p| p.guests.maybe_default_expiry_days = (days != u32::MAX).then_some(days));
          }
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-sharing")}</legend>
        <SelectRow
          id="policy-history"
          label=Signal::derive(move || i18n.t("policy-history"))
          options=history_options
          selected=Signal::derive(move || match draft.get().history_default {
            HistoryPolicy::Full => "full".to_owned(),
            _ => "from_join".to_owned(),
          })
          on_select=move |value: String| {
            policy.edit(|p| {
              p.history_default = if value == "full" { HistoryPolicy::Full } else { HistoryPolicy::FromJoin };
            });
          }
        />
      </fieldset>

      <PolicyFooter policy=policy />
    </form>
  }
}

/// "a.example, B.example ; c.example" becomes lower-case host names without `@`.
pub fn parse_domains(text: &str) -> Vec<String> {
  text
    .split([',', ';', ' ', '\n'])
    .map(|part| part.trim().trim_start_matches('@').to_lowercase())
    .filter(|part| !part.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn domains_are_split_cleaned_and_lowercased() {
    assert_eq!(
      parse_domains("Example.com, @partner.example ;  c.example\n"),
      vec!["example.com", "partner.example", "c.example"]
    );
    assert!(parse_domains("  ,, ").is_empty());
  }
}
