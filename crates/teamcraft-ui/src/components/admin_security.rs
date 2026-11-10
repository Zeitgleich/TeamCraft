use leptos::prelude::*;
use teamcraft_proto::{AdminSelfJoin, GuestDmStart, HistoryPolicy, LeaverRemovalMode, ScreenShareResolution};

use crate::components::admin_console::SectionHeader;
use crate::components::admin_owners::OwnersPanel;
use crate::components::admin_policy::{
  CheckRow, PolicyBlockerNote, PolicyDraft, PolicyFooter, SelectRow, use_policy_draft,
};
use crate::components::people_picker::PeoplePicker;
use crate::i18n::use_i18n;
use crate::model::{User, UserId};
use crate::state::use_app_state;

/// Session lengths offered, in hours; `0` stands for "no limit".
const SESSION_HOURS: [u32; 5] = [24, 7 * 24, 30 * 24, 90 * 24, 0];
const IDLE_MINUTES: [u32; 5] = [15, 60, 120, 480, 0];
const PASSWORD_CHARS: [u32; 5] = [0, 10, 12, 14, 16];
const GUEST_EXPIRY_DAYS: [u32; 6] = [u32::MAX, 7, 30, 90, 180, 0];
/// The notice schedules on offer, slug first; the slug is what the select keeps.
const GUEST_NOTICE_SCHEDULES: [(&str, &[u32]); 4] = [
  ("default", &[14, 3]),
  ("long", &[30, 14, 3]),
  ("short", &[7, 1]),
  ("off", &[]),
];
const SPONSOR_EXTENSION_DAYS: [u32; 6] = [120, 30, 90, 180, 365, 0];
/// Idle local guest accounts are deactivated this many days after their last space; `u32::MAX` never.
const GUEST_DEACTIVATE_DAYS: [u32; 5] = [u32::MAX, 14, 30, 90, 180];
/// Inactive channels (issue #212): days of quiet before the owners are told.
const INACTIVITY_NOTICE_DAYS: [u32; 5] = [90, 180, 365, 540, 730];
/// Days between the notice and the archiving; `0`: notices only.
const INACTIVITY_ARCHIVE_DAYS: [u32; 5] = [0, 14, 30, 60, 90];
const INACTIVITY_SNOOZE_DAYS: [u32; 5] = [30, 60, 90, 180, 365];
const INACTIVITY_MAX_PER_RUN: [u32; 5] = [5, 10, 25, 50, 100];
/// Days after the archiving that a channel is deleted; `u32::MAX` never.
const INACTIVITY_DELETE_DAYS: [u32; 5] = [u32::MAX, 30, 90, 180, 365];
const APPROVALS: [u8; 2] = [2, 3];
/// The longest voice message offered, in minutes.
const VOICE_MINUTES: [u16; 6] = [1, 2, 5, 10, 30, 60];
/// The grace before a leaver is removed (docs/provisioning.md §3): at once, a quarter of an hour, an
/// hour (the default), four hours, a day, a week.
const LEAVER_GRACE_MINUTES: [u32; 6] = [0, 15, 60, 240, 1440, 10_080];
/// Automatic removals per admin device and day.
const LEAVER_MAX_PER_DAY: [u32; 6] = [1, 5, 10, 25, 50, 100];

/// Security policy: the signed settings that the server and every member client enforce.
#[component]
pub fn SecuritySection() -> impl IntoView {
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;

  let leaver_grace_options = Signal::derive(move || {
    LEAVER_GRACE_MINUTES
      .into_iter()
      .map(|minutes| {
        let label = match minutes {
          0 => i18n.t("policy-leavers-at-once"),
          minutes if minutes % 1440 == 0 => i18n.t_count("policy-days", (minutes / 1440) as usize),
          minutes if minutes % 60 == 0 => i18n.t_count("policy-hours", (minutes / 60) as usize),
          minutes => i18n.t_count("policy-minutes", minutes as usize),
        };
        (minutes.to_string(), label)
      })
      .collect()
  });
  let leaver_limit_options = Signal::derive(move || {
    LEAVER_MAX_PER_DAY
      .into_iter()
      .map(|count| {
        (
          count.to_string(),
          i18n.t_count("policy-leavers-people", count as usize),
        )
      })
      .collect()
  });
  let voice_options = Signal::derive(move || {
    VOICE_MINUTES
      .into_iter()
      .map(|minutes| {
        (
          minutes.to_string(),
          i18n.t_count("policy-minutes", minutes as usize),
        )
      })
      .collect()
  });
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
  let notice_options = Signal::derive(move || {
    GUEST_NOTICE_SCHEDULES
      .iter()
      .map(|(slug, _)| {
        (
          (*slug).to_owned(),
          i18n.t(&format!("policy-guest-notices-{slug}")),
        )
      })
      .collect()
  });
  let sponsor_options = Signal::derive(move || {
    SPONSOR_EXTENSION_DAYS
      .into_iter()
      .map(|days| {
        let label = match days {
          0 => i18n.t("policy-guest-sponsor-none"),
          days => i18n.t_count("policy-guest-sponsor-days", days as usize),
        };
        (days.to_string(), label)
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
  let days_options = |values: &'static [u32], special: Option<(u32, &'static str)>| {
    Signal::derive(move || {
      values
        .iter()
        .map(|days| {
          let label = match special {
            Some((value, key)) if value == *days => i18n.t(key),
            _ => i18n.t_count("policy-days", *days as usize),
          };
          (days.to_string(), label)
        })
        .collect::<Vec<_>>()
    })
  };
  let guest_deactivate_options = days_options(
    &GUEST_DEACTIVATE_DAYS,
    Some((u32::MAX, "policy-guest-deactivate-never")),
  );
  let inactivity_notice_options = days_options(&INACTIVITY_NOTICE_DAYS, None);
  let inactivity_archive_options = days_options(
    &INACTIVITY_ARCHIVE_DAYS,
    Some((0, "policy-inactivity-notice-only")),
  );
  let inactivity_snooze_options = days_options(&INACTIVITY_SNOOZE_DAYS, None);
  let inactivity_limit_options = Signal::derive(move || {
    INACTIVITY_MAX_PER_RUN
      .into_iter()
      .map(|count| {
        (
          count.to_string(),
          i18n.t_count("policy-inactivity-channels", count as usize),
        )
      })
      .collect::<Vec<_>>()
  });
  let inactivity_delete_options = days_options(
    &INACTIVITY_DELETE_DAYS,
    Some((u32::MAX, "policy-inactivity-delete-never")),
  );
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
        <legend>{move || i18n.t("policy-screen-share")}</legend>
        <CheckRow
          name="screen_share_uhd"
          label=Signal::derive(move || i18n.t("policy-screen-share-uhd"))
          hint=Signal::derive(move || i18n.t("policy-screen-share-uhd-hint"))
          checked=Signal::derive(move || draft.get().screen_share.max_resolution == ScreenShareResolution::Uhd)
          on_change=move |value| {
            policy.edit(|p| {
              p.screen_share.max_resolution = if value {
                ScreenShareResolution::Uhd
              } else {
                ScreenShareResolution::Standard
              };
            })
          }
        />
      </fieldset>

      <BugReportRecipients policy=policy />

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-voice-messages")}</legend>
        <CheckRow
          name="voice_messages_enabled"
          label=Signal::derive(move || i18n.t("policy-voice-messages-enabled"))
          hint=Signal::derive(move || i18n.t("policy-voice-messages-enabled-hint"))
          checked=Signal::derive(move || draft.get().voice_messages.enabled)
          on_change=move |value| policy.edit(|p| p.voice_messages.enabled = value)
        />
        <SelectRow
          id="policy-voice-max"
          label=Signal::derive(move || i18n.t("policy-voice-messages-max"))
          options=voice_options
          selected=Signal::derive(move || draft.get().voice_messages.max_minutes.to_string())
          disabled=Signal::derive(move || !draft.get().voice_messages.enabled)
          on_select=move |value: String| {
            if let Ok(minutes) = value.parse::<u16>() {
              policy.edit(|p| p.voice_messages.max_minutes = minutes);
            }
          }
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-leavers")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-leavers-hint")}</p>
        <CheckRow
          name="leavers_automatic"
          label=Signal::derive(move || i18n.t("policy-leavers-automatic"))
          hint=Signal::derive(move || i18n.t("policy-leavers-automatic-hint"))
          checked=Signal::derive(move || draft.get().leavers.mode == LeaverRemovalMode::Automatic)
          on_change=move |value| {
            policy.edit(|p| {
              p.leavers.mode = if value { LeaverRemovalMode::Automatic } else { LeaverRemovalMode::Manual };
            })
          }
        />
        <SelectRow
          id="policy-leavers-grace"
          label=Signal::derive(move || i18n.t("policy-leavers-grace"))
          options=leaver_grace_options
          selected=Signal::derive(move || draft.get().leavers.grace_minutes().to_string())
          disabled=Signal::derive(move || draft.get().leavers.mode != LeaverRemovalMode::Automatic)
          on_select=move |value: String| {
            if let Ok(minutes) = value.parse::<u32>() {
              policy.edit(|p| p.leavers.grace_minutes = minutes);
            }
          }
        />
        <SelectRow
          id="policy-leavers-limit"
          label=Signal::derive(move || i18n.t("policy-leavers-limit"))
          options=leaver_limit_options
          selected=Signal::derive(move || draft.get().leavers.max_per_day().to_string())
          disabled=Signal::derive(move || draft.get().leavers.mode != LeaverRemovalMode::Automatic)
          on_select=move |value: String| {
            if let Ok(count) = value.parse::<u32>() {
              policy.edit(|p| p.leavers.max_per_day = count);
            }
          }
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
        <SelectRow
          id="policy-guest-notices"
          label=Signal::derive(move || i18n.t("policy-guest-notices"))
          options=notice_options
          selected=Signal::derive(move || {
            let days = draft.get().guests.effective_notice_days();
            GUEST_NOTICE_SCHEDULES
              .iter()
              .find(|(_, schedule)| *schedule == days.as_slice())
              .map_or_else(|| "default".to_owned(), |(slug, _)| (*slug).to_owned())
          })
          on_select=move |value: String| {
            let days = GUEST_NOTICE_SCHEDULES
              .iter()
              .find(|(slug, _)| *slug == value)
              .map(|(_, schedule)| schedule.to_vec());
            policy.edit(|p| p.guests.maybe_expiry_notice_days = days);
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-guest-notices-hint")}</p>
        <SelectRow
          id="policy-guest-sponsor-extension"
          label=Signal::derive(move || i18n.t("policy-guest-sponsor-extension"))
          options=sponsor_options
          selected=Signal::derive(move || draft.get().guests.effective_sponsor_extension_days().to_string())
          on_select=move |value: String| {
            policy.edit(|p| p.guests.maybe_sponsor_extension_days = value.parse().ok());
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-guest-sponsor-extension-hint")}</p>
        <SelectRow
          id="policy-guest-deactivate"
          label=Signal::derive(move || i18n.t("policy-guest-deactivate"))
          options=guest_deactivate_options
          selected=Signal::derive(move || {
            draft.get().guests.maybe_deactivate_accounts_after_days.unwrap_or(u32::MAX).to_string()
          })
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(u32::MAX);
            policy.edit(|p| p.guests.maybe_deactivate_accounts_after_days = (days != u32::MAX).then_some(days));
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-guest-deactivate-hint")}</p>
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-inactivity")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-inactivity-hint")}</p>
        <CheckRow
          name="inactivity_enabled"
          label=Signal::derive(move || i18n.t("policy-inactivity-enable"))
          hint=Signal::derive(move || i18n.t("policy-inactivity-enable-hint"))
          checked=Signal::derive(move || draft.get().inactivity.is_enabled)
          on_change=move |value| policy.edit(|p| p.inactivity.is_enabled = value)
        />
        <SelectRow
          id="policy-inactivity-notice"
          label=Signal::derive(move || i18n.t("policy-inactivity-notice"))
          options=inactivity_notice_options
          selected=Signal::derive(move || draft.get().inactivity.notice_after_days.to_string())
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(teamcraft_proto::DEFAULT_INACTIVITY_NOTICE_DAYS);
            policy.edit(|p| p.inactivity.notice_after_days = days);
          }
        />
        <SelectRow
          id="policy-inactivity-archive"
          label=Signal::derive(move || i18n.t("policy-inactivity-archive"))
          options=inactivity_archive_options
          selected=Signal::derive(move || draft.get().inactivity.archive_after_more_days.to_string())
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(teamcraft_proto::DEFAULT_INACTIVITY_ARCHIVE_DAYS);
            policy.edit(|p| p.inactivity.archive_after_more_days = days);
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-inactivity-archive-hint")}</p>
        <SelectRow
          id="policy-inactivity-snooze"
          label=Signal::derive(move || i18n.t("policy-inactivity-snooze"))
          options=inactivity_snooze_options
          selected=Signal::derive(move || draft.get().inactivity.snooze_days.to_string())
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(teamcraft_proto::DEFAULT_INACTIVITY_SNOOZE_DAYS);
            policy.edit(|p| p.inactivity.snooze_days = days);
          }
        />
        <SelectRow
          id="policy-inactivity-limit"
          label=Signal::derive(move || i18n.t("policy-inactivity-limit"))
          options=inactivity_limit_options
          selected=Signal::derive(move || draft.get().inactivity.max_per_run.to_string())
          on_select=move |value: String| {
            let count = value.parse::<u32>().unwrap_or(teamcraft_proto::DEFAULT_INACTIVITY_MAX_PER_RUN);
            policy.edit(|p| p.inactivity.max_per_run = count);
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-inactivity-limit-hint")}</p>
        <SelectRow
          id="policy-inactivity-delete"
          label=Signal::derive(move || i18n.t("policy-inactivity-delete"))
          options=inactivity_delete_options
          selected=Signal::derive(move || {
            draft.get().inactivity.maybe_delete_after_archived_days.unwrap_or(u32::MAX).to_string()
          })
          on_select=move |value: String| {
            let days = value.parse::<u32>().unwrap_or(u32::MAX);
            policy.edit(|p| p.inactivity.maybe_delete_after_archived_days = (days != u32::MAX).then_some(days));
          }
        />
        <p class="muted-text small">{move || i18n.t("policy-inactivity-delete-hint")}</p>
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

/// Who receives "Report a problem" reports (docs/bug-reports.md): the owners and admins, or the
/// people picked here. Part of the signed policy, so the server can't redirect reports.
#[component]
fn BugReportRecipients(policy: PolicyDraft) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let draft = policy.draft;
  let is_named = RwSignal::new(!draft.get_untracked().bug_reports.recipients.is_empty());
  let chosen = RwSignal::new(Vec::<User>::new());
  // The draft leads (it is reset after a save or a discard); the picker follows it.
  Effect::new(move |_| {
    let ids = draft.with(|p| p.bug_reports.recipients.clone());
    if chosen.with_untracked(|users| user_ids(users)) != ids {
      let users: Vec<User> = ids
        .iter()
        .filter_map(|id| state.read(|backend| backend.user(*id)))
        .collect();
      chosen.set(users);
      is_named.set(!ids.is_empty() || is_named.get_untracked());
    }
  });
  // Picks go into the draft; people the directory doesn't know here stay listed.
  Effect::new(move |_| {
    let picked = chosen.with(|users| user_ids(users));
    let current = draft.with_untracked(|p| p.bug_reports.recipients.clone());
    let unknown = current
      .iter()
      .filter(|id| state.read(|backend| backend.user(**id)).is_none());
    let next: Vec<UserId> = picked.iter().chain(unknown).copied().collect();
    if next != current {
      policy.edit(|p| p.bug_reports.recipients = next);
    }
  });
  let choose_admins = move |_| {
    is_named.set(false);
    chosen.set(Vec::new());
  };
  view! {
    <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
      <legend>{move || i18n.t("policy-bug-reports")}</legend>
      <p class="muted-text small">{move || i18n.t("policy-bug-reports-hint")}</p>
      <label class="radio">
        <input type="radio" name="bug_report_recipients" value="admins" prop:checked=move || !is_named.get() on:change=choose_admins />
        <span>
          <strong>{move || i18n.t("policy-bug-reports-admins")}</strong>
          <span class="muted-text small">{move || i18n.t("policy-bug-reports-admins-hint")}</span>
        </span>
      </label>
      <label class="radio">
        <input type="radio" name="bug_report_recipients" value="named" prop:checked=move || is_named.get() on:change=move |_| is_named.set(true) />
        <span>
          <strong>{move || i18n.t("policy-bug-reports-named")}</strong>
          <span class="muted-text small">{move || i18n.t("policy-bug-reports-named-hint")}</span>
        </span>
      </label>
      <Show when=move || is_named.get() fallback=|| ()>
        <PeoplePicker chosen=chosen chosen_label_key="policy-bug-reports-chosen" allows_guest_accounts=false />
      </Show>
    </fieldset>
  }
}

fn user_ids(users: &[User]) -> Vec<UserId> {
  users.iter().map(|user| user.id).collect()
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
