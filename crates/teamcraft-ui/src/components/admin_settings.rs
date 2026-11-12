use std::collections::BTreeMap;

use leptos::prelude::*;
use serde_json::{Map, Value, json};
use teamcraft_proto::{
  Dsn, HOLIDAY_REGIONS, HolidayFeedError, HolidayFeedStatus, RuntimeSettingsKey, RuntimeSettingsResponse,
  RuntimeSettingsSection, SettingSource, Snowflake, check_bridge_public_url, is_crash_environment,
};

use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  CheckField, FieldErrors, SourceBadge, TextAreaField, TextField, format_scaled, is_web_url, list_text,
  parse_list, parse_scaled,
};
use crate::form_errors::focus_first_invalid;
use crate::i18n::use_i18n;
use crate::meetings::holiday_region_key;
use crate::state::use_app_state;

const MIB: u64 = 1_048_576;
const MINUTE: u64 = 60;
/// Form values by field id: booleans as `true` / `false`, numbers in the shown unit, lists as text.
type Values = BTreeMap<&'static str, String>;

/// Groups in the order of the page.
const GROUPS: [&str; 15] = [
  "server_rules",
  "notifications",
  "email_digest",
  "shares",
  "limits",
  "integration",
  "local_accounts",
  "sign_in_page",
  "mail_texts",
  "meeting_mail",
  "token_binding",
  "app_links",
  "health_alerts",
  "holidays",
  "crash_reports",
];
/// The sign-in methods an admin can choose, as the form's values: comma-separated, in order.
const METHOD_CHOICES: [&str; 4] = ["sso,local", "local,sso", "sso", "local"];
/// The notification levels an instance default can be (`none` would silence everybody).
const LEVEL_CHOICES: [&str; 2] = ["all", "mentions"];
/// The policies for binding tokens to devices.
const DPOP_POLICY_CHOICES: [&str; 3] = ["optional", "required", "off"];
/// Where crash reports go (docs/crash-reports.md).
const CRASH_MODE_CHOICES: [&str; 3] = ["publisher", "own", "off"];

/// How a field is edited and checked.
#[derive(Clone, Copy)]
enum Kind {
  Bool,
  /// A whole number of raw units, shown as `raw / scale` in the unit named by the i18n key.
  Number {
    scale: u64,
    max: u64,
    unit: Option<&'static str>,
    /// Empty means none (off).
    is_optional: bool,
    /// The smallest raw value the server takes.
    min: u64,
    /// What the field's own message says when the value is out of range, naming the range.
    maybe_range_error: Option<&'static str>,
  },
  Text,
  Area,
  /// Space or channel IDs.
  IdList,
  /// Other identifiers, checked by the server.
  TextList,
  /// The sign-in methods in order, one of [`METHOD_CHOICES`].
  Methods,
  /// A notification level, one of [`LEVEL_CHOICES`].
  Level,
  /// A policy for binding tokens to devices, one of [`DPOP_POLICY_CHOICES`].
  DpopPolicy,
  /// The region of the holiday table, one of [`HOLIDAY_REGIONS`]; empty means none.
  HolidayRegion,
  /// One language's entry of a text map (`help_text.de`); empty means none.
  MapText,
  /// An https link that may be empty (none).
  Link,
  /// The public address of the integration bridge (docs/bridge.md): https, or http for localhost;
  /// no credentials, query or fragment; empty means none.
  BridgeUrl,
  /// Where crash reports go, one of [`CRASH_MODE_CHOICES`].
  CrashMode,
  /// A crash reports DSN (docs/crash-reports.md); empty unless reports go to an own backend.
  Dsn,
  /// A crash reports environment name: lowercase letters, digits, `-`, `_`, `.`.
  Environment,
}

#[derive(Clone, Copy)]
struct FieldDef {
  id: &'static str,
  kind: Kind,
}

const fn flag(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Bool }
}

const fn count(id: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale: 1,
      max: u32::MAX as u64,
      unit: None,
      is_optional: false,
      min: 0,
      maybe_range_error: None,
    },
  }
}

const fn amount(id: &'static str, scale: u64, max: u64, unit: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale,
      max,
      unit: Some(unit),
      is_optional: false,
      min: 0,
      maybe_range_error: None,
    },
  }
}

/// Like [`amount`], with a smallest value the server takes and a message that names the range.
const fn ranged(
  id: &'static str,
  scale: u64,
  range: (u64, u64),
  unit: &'static str,
  range_error: &'static str,
) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale,
      max: range.1,
      unit: Some(unit),
      is_optional: false,
      min: range.0,
      maybe_range_error: Some(range_error),
    },
  }
}

/// Like [`amount`], but may be left empty (none).
const fn optional_amount(id: &'static str, scale: u64, max: u64, unit: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale,
      max,
      unit: Some(unit),
      is_optional: true,
      min: 0,
      maybe_range_error: None,
    },
  }
}

const fn text(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Text }
}

const fn area(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Area }
}

const fn map_text(id: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::MapText,
  }
}

const SECONDS: &str = "settings-unit-seconds";
const ENTRIES: &str = "settings-unit-entries";
const MINUTES: &str = "settings-unit-minutes";
const HOURS: &str = "settings-unit-hours";
const DAYS: &str = "settings-unit-days";
const PERCENT: &str = "settings-unit-percent";
const MILLISECONDS: &str = "settings-unit-ms";
const REPORTS: &str = "settings-unit-reports";
const MEGABYTES: &str = "settings-unit-mib";
const KILOBYTES: &str = "settings-unit-kib";
const PIXELS: &str = "settings-unit-px";
const KIB: u64 = 1024;
const U32_MAX: u64 = u32::MAX as u64;
const U64_MAX: u64 = 9_000_000_000_000_000;

const SERVER_RULES: &[FieldDef] = &[
  flag("invites_require_verified_email"),
  FieldDef {
    id: "default_spaces",
    kind: Kind::IdList,
  },
  FieldDef {
    id: "default_channels",
    kind: Kind::IdList,
  },
  optional_amount("remove_deactivated_after_days", 1, 3_650, DAYS),
  amount("admin_join_request_days", 1, 90, DAYS),
  flag("one_to_one_call_ends_for_both"),
];
const TOKEN_BINDING: &[FieldDef] = &[
  FieldDef {
    id: "dpop_policy",
    kind: Kind::DpopPolicy,
  },
  amount("proof_max_age_secs", 1, 3_600, SECONDS),
  amount("proof_leeway_secs", 1, 3_600, SECONDS),
  amount("nonce_rotation_secs", 1, 86_400, SECONDS),
  amount("replay_cache_max_entries", 1, 10_000_000, ENTRIES),
  amount("replay_cache_max_entries_per_user", 1, 10_000_000, ENTRIES),
];
const APP_LINKS: &[FieldDef] = &[
  flag("is_enabled"),
  FieldDef {
    id: "android_package_names",
    kind: Kind::TextList,
  },
  FieldDef {
    id: "android_cert_fingerprints",
    kind: Kind::TextList,
  },
  text("apple_team_id"),
  text("apple_bundle_id"),
];
const CRASH_REPORTS: &[FieldDef] = &[
  FieldDef {
    id: "mode",
    kind: Kind::CrashMode,
  },
  FieldDef {
    id: "dsn",
    kind: Kind::Dsn,
  },
  ranged(
    "sample_percent",
    1,
    (0, 100),
    PERCENT,
    "settings-error-crash-percent",
  ),
  ranged(
    "trace_percent",
    1,
    (0, 100),
    PERCENT,
    "settings-error-crash-percent",
  ),
  ranged(
    "slow_job_ms",
    1,
    (0, 60_000),
    MILLISECONDS,
    "settings-error-slow-job",
  ),
  ranged(
    "call_reports_per_call",
    1,
    (0, 50),
    REPORTS,
    "settings-error-call-reports",
  ),
  FieldDef {
    id: "environment",
    kind: Kind::Environment,
  },
  flag("allows_opt_out"),
  flag("shows_notice"),
];
const HEALTH_ALERTS: &[FieldDef] = &[
  flag("is_enabled"),
  ranged(
    "hold_secs",
    MINUTE,
    (MINUTE, 86_400),
    MINUTES,
    "settings-error-health-minutes",
  ),
  ranged(
    "clear_secs",
    MINUTE,
    (MINUTE, 86_400),
    MINUTES,
    "settings-error-health-minutes",
  ),
  ranged(
    "backup_max_age_secs",
    3_600,
    (3_600, 30 * 86_400),
    HOURS,
    "settings-error-health-backup-hours",
  ),
  ranged(
    "certificate_min_days",
    1,
    (1, 365),
    DAYS,
    "settings-error-health-days",
  ),
  ranged(
    "disk_max_percent",
    1,
    (50, 99),
    PERCENT,
    "settings-error-health-percent",
  ),
  flag("is_email_enabled"),
  ranged(
    "history_days",
    1,
    (1, 3650),
    DAYS,
    "settings-error-health-history-days",
  ),
  ranged("secret_min_days", 1, (1, 365), DAYS, "settings-error-health-days"),
  flag("is_probing_enabled"),
  ranged(
    "probe_interval_secs",
    1,
    (10, 3_600),
    SECONDS,
    "settings-error-health-probe-interval",
  ),
  ranged(
    "probe_timeout_secs",
    1,
    (1, 60),
    SECONDS,
    "settings-error-health-probe-timeout",
  ),
  ranged(
    "call_max_loss_percent",
    1,
    (1, 50),
    PERCENT,
    "settings-error-health-call-loss",
  ),
  ranged(
    "call_max_fallback_percent",
    1,
    (10, 100),
    PERCENT,
    "settings-error-health-call-fallback",
  ),
  ranged(
    "call_window_secs",
    MINUTE,
    (MINUTE, 3_600),
    MINUTES,
    "settings-error-health-call-window",
  ),
];
const HOLIDAYS: &[FieldDef] = &[
  FieldDef {
    id: "region",
    kind: Kind::HolidayRegion,
  },
  FieldDef {
    id: "feed_url",
    kind: Kind::Link,
  },
  ranged(
    "feed_refresh_secs",
    3_600,
    (3_600, 7 * 86_400),
    HOURS,
    "settings-error-holiday-refresh-hours",
  ),
  ranged(
    "feed_max_age_secs",
    86_400,
    (86_400, 90 * 86_400),
    DAYS,
    "settings-error-holiday-max-age-days",
  ),
];
const NOTIFICATIONS: &[FieldDef] = &[
  flag("read_receipts_default"),
  FieldDef {
    id: "default_level",
    kind: Kind::Level,
  },
  amount("urgent_repeat_interval_secs", 1, U64_MAX, SECONDS),
  count("urgent_max_pushes"),
  amount("mobile_push_delay_secs", 1, U64_MAX, SECONDS),
  flag("is_desktop_attention_enabled"),
  flag("is_admins_mention_enabled"),
];
const EMAIL_DIGEST: &[FieldDef] = &[
  flag("is_enabled"),
  amount("default_interval_secs", MINUTE, U64_MAX, MINUTES),
  amount("min_interval_secs", MINUTE, U64_MAX, MINUTES),
  amount("grace_secs", MINUTE, U64_MAX, MINUTES),
  text("subject_one"),
  text("subject_many"),
  area("body_one"),
  area("body_many"),
];
const SHARES: &[FieldDef] = &[
  flag("is_enabled"),
  amount("default_expiry_days", 1, U32_MAX, DAYS),
  amount("max_expiry_days", 1, U32_MAX, DAYS),
  count("max_downloads_cap"),
  count("max_links_per_user"),
];
const LIMITS: &[FieldDef] = &[
  amount("max_attachment_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_attachment_bytes_per_user", MIB, U64_MAX, MEGABYTES),
  amount("max_avatar_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_space_image_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_background_bytes", MIB, U64_MAX, MEGABYTES),
  count("max_backgrounds_per_user"),
  count("max_group_dm_participants"),
  count("max_user_group_members"),
  count("max_meeting_mails_per_user_per_hour"),
  count("max_large_mentions_per_user_per_hour"),
  count("max_custom_emoji"),
  amount(
    "max_custom_emoji_bytes",
    KIB,
    teamcraft_proto::MAX_CUSTOM_EMOJI_BYTES_LIMIT,
    KILOBYTES,
  ),
  amount(
    "max_custom_emoji_side_px",
    1,
    teamcraft_proto::MAX_CUSTOM_EMOJI_SIDE_PX_LIMIT as u64,
    PIXELS,
  ),
];
const INTEGRATION: &[FieldDef] = &[
  amount("default_retention_days", 1, U32_MAX, DAYS),
  FieldDef {
    id: "bridge_public_url",
    kind: Kind::BridgeUrl,
  },
];
const LOCAL_ACCOUNTS: &[FieldDef] = &[
  flag("is_enabled"),
  flag("require_totp"),
  amount("activation_ttl_hours", 1, U32_MAX, HOURS),
  amount("session_ttl_hours", 1, U32_MAX, HOURS),
  count("max_failed_attempts"),
  amount("lockout_secs", MINUTE, U64_MAX, MINUTES),
];
const SIGN_IN_PAGE: &[FieldDef] = &[
  FieldDef {
    id: "methods",
    kind: Kind::Methods,
  },
  map_text("help_text.en"),
  map_text("help_text.de"),
  FieldDef {
    id: "help_link",
    kind: Kind::Link,
  },
  flag("is_login_page_always_shown"),
];
const MAIL_TEXTS: &[FieldDef] = &[
  text("invite_subject"),
  area("invite_body"),
  text("activation_subject"),
  area("activation_body"),
];
const MEETING_MAIL: &[FieldDef] = &[
  count("max_invitees"),
  text("invitation_subject"),
  area("invitation_body"),
  text("update_subject"),
  area("update_body"),
  text("cancellation_subject"),
  area("cancellation_body"),
];

/// Instance settings that used to need the configuration file: one form per group, each with its
/// own save and a way back to the file's value.
#[component]
pub fn SettingsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let loaded = Memo::new(move |_| state.read(|backend| backend.runtime_settings().is_some()));
  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-settings-title"))
      subtitle=Signal::derive(move || i18n.t("admin-settings-subtitle"))
    />
    {move || (!loaded.get()).then(|| view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> })}
    <div class="policy-form">
      {GROUPS.into_iter().map(|slug| view! { <GroupForm slug=slug /> }).collect_view()}
      <crate::components::admin_group_mappings::GroupMappingsForm />
    </div>
  }
}

#[component]
fn GroupForm(slug: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let key = RuntimeSettingsKey::from(slug.to_owned());
  let stored = {
    let key = key.clone();
    Memo::new(move |_| {
      state
        .read(|backend| backend.runtime_settings())
        .map(|response| to_values(&key, &response))
    })
  };
  let draft = RwSignal::new(stored.get_untracked().unwrap_or_default());
  let show_errors = RwSignal::new(false);
  Effect::new(move |previous: Option<()>| {
    let latest = stored.get();
    // The first run only subscribes; later changes of the stored values replace the draft.
    if previous.is_some()
      && let Some(values) = latest
    {
      draft.set(values);
      show_errors.set(false);
    }
  });
  let source = {
    let key = key.clone();
    Memo::new(move |_| {
      state
        .read(|backend| backend.runtime_settings())
        .and_then(|response| {
          response
            .sources
            .into_iter()
            .find(|state| state.key == key)
            .map(|state| state.source)
        })
        .unwrap_or(SettingSource::Config)
    })
  };
  let group_error = Signal::derive({
    let key = key.clone();
    move || {
      show_errors
        .get()
        .then(|| draft.with(|values| check(&key, values).err()))
        .flatten()
        .filter(|errors| errors.iter().any(|(field, _)| *field == "form"))
        .map(|_| "admin-error-invalid")
    }
  });
  let save = {
    let key = key.clone();
    move |ev: web_sys::SubmitEvent| {
      ev.prevent_default();
      show_errors.set(true);
      match draft.with_untracked(|values| check(&key, values)) {
        Ok(section) => state.write(|backend| backend.save_runtime_settings(section)),
        Err(_) => focus_first_invalid(form_id(slug)),
      }
    }
  };
  let reset_key = key.clone();
  let title = format!("settings-group-{slug}");
  let hint = format!("settings-group-{slug}-hint");
  view! {
    <form id=form_id(slug) on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t(&title)}</legend>
        <p class="settings-source small">
          <SourceBadge source=source />
          <span class="muted-text">{move || i18n.t(&hint)}</span>
        </p>
        {fields_of(slug)
          .iter()
          .map(|field| view! { <FieldRow slug=slug field=*field draft=draft show_errors=show_errors /> })
          .collect_view()}
        {(slug == "holidays").then(|| view! { <HolidayFeedLine /> })}
        {move || group_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
        <div class="policy-footer">
          <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button>
          <button
            type="button"
            class="btn"
            prop:disabled=move || source.get() != SettingSource::Database
            on:click=move |_| {
              let key = reset_key.clone();
              state.write(|backend| backend.reset_runtime_settings(key));
            }
          >
            {move || i18n.t("admin-reset-to-config")}
          </button>
        </div>
      </fieldset>
    </form>
  }
}

#[component]
fn FieldRow(
  slug: &'static str,
  field: FieldDef,
  draft: RwSignal<Values>,
  show_errors: RwSignal<bool>,
) -> impl IntoView {
  let id = field.id;
  let dom_id = format!("settings-{slug}-{}", id.replace('.', "-"));
  let label = leak_key(label_key(id));
  let value = Signal::derive(move || draft.with(|values| values.get(id).cloned().unwrap_or_default()));
  let on_input = Callback::new(move |text: String| {
    draft.update(|values| {
      values.insert(id, text);
    });
  });
  let key = RuntimeSettingsKey::from(slug.to_owned());
  let error = Signal::derive(move || {
    if !show_errors.get() {
      return None;
    }
    draft.with(|values| {
      check(&key, values).err().and_then(|errors| {
        errors
          .into_iter()
          .find(|(field, _)| *field == id)
          .map(|(_, key)| key)
      })
    })
  });
  match field.kind {
    Kind::Bool => view! {
      <CheckField
        label=label
        checked=Signal::derive(move || value.get() == "true")
        on_change=Callback::new(move |checked: bool| on_input.run(checked.to_string()))
      />
    }
    .into_any(),
    Kind::Number { unit, .. } => view! {
      <TextField id=leak_key(dom_id) label=label input_type="number" unit=unit.unwrap_or_default() value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Text | Kind::Environment => view! {
      <TextField id=leak_key(dom_id) label=label value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Dsn => view! {
      <TextField id=leak_key(dom_id) label=label input_type="url" placeholder="https://key@errors.example.com/1" value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Area | Kind::MapText => view! {
      <TextAreaField id=leak_key(dom_id) label=label rows=3 value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Link => {
      let placeholder = if id == "feed_url" {
        "https://calendar.example.com/holidays.ics"
      } else {
        "https://help.example.com"
      };
      view! {
        <TextField id=leak_key(dom_id) label=label input_type="url" placeholder=placeholder value=value on_input=on_input error=error />
      }
      .into_any()
    },
    Kind::BridgeUrl => view! {
      <TextField
        id=leak_key(dom_id)
        label=label
        input_type="url"
        placeholder="https://bridge.example.com"
        hint="settings-hint-bridge_public_url"
        value=value
        on_input=on_input
        error=error
      />
    }
    .into_any(),
    Kind::Methods => {
      let i18n = use_i18n();
      let dom_id = leak_key(dom_id);
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {METHOD_CHOICES
              .into_iter()
              .map(|choice| {
                let key = method_choice_key(choice);
                view! { <option value=choice selected=move || value.get() == choice>{move || i18n.t(key)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::Level => {
      let i18n = use_i18n();
      let dom_id = leak_key(dom_id);
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {LEVEL_CHOICES
              .into_iter()
              .map(|choice| {
                let key = level_choice_key(choice);
                view! { <option value=choice selected=move || value.get() == choice>{move || i18n.t(key)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::DpopPolicy => {
      let i18n = use_i18n();
      let dom_id = leak_key(dom_id);
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {DPOP_POLICY_CHOICES
              .into_iter()
              .map(|choice| {
                let key = dpop_policy_choice_key(choice);
                view! { <option value=choice selected=move || value.get() == choice>{move || i18n.t(key)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::CrashMode => {
      let i18n = use_i18n();
      let state = use_app_state();
      let dom_id = leak_key(dom_id);
      // Where the publisher's service is, so the admin sees where reports go by default.
      let publisher_host = Memo::new(move |_| {
        state
          .read(|backend| backend.runtime_settings())
          .and_then(|response| response.maybe_publisher_crash_reports_host)
      });
      let choice_text = move |choice: &'static str| match (choice, publisher_host.get()) {
        ("publisher", Some(host)) => i18n.t_with("settings-crash-mode-publisher", &[("host", host.as_str())]),
        ("publisher", None) => i18n.t("settings-crash-mode-publisher-none"),
        ("own", _) => i18n.t("settings-crash-mode-own"),
        _ => i18n.t("settings-crash-mode-off"),
      };
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {CRASH_MODE_CHOICES
              .into_iter()
              .map(|choice| {
                view! { <option value=choice selected=move || value.get() == choice>{move || choice_text(choice)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::HolidayRegion => {
      let i18n = use_i18n();
      let dom_id = leak_key(dom_id);
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {std::iter::once("")
              .chain(HOLIDAY_REGIONS)
              .map(|choice| {
                let key = leak_key(holiday_region_key(choice));
                view! { <option value=choice selected=move || value.get() == choice>{move || i18n.t(key)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::IdList | Kind::TextList => view! {
      <TextAreaField
        id=leak_key(dom_id)
        label=label
        hint="settings-list-hint"
        rows=2
        value=value
        on_input=on_input
        error=error
      />
    }
    .into_any(),
  }
}

/// The i18n key of a field's label (Fluent names have no dots).
fn label_key(id: &str) -> String {
  format!("settings-f-{}", id.replace('.', "-"))
}

/// The i18n key of a choice of sign-in methods.
fn method_choice_key(choice: &str) -> &'static str {
  match choice {
    "local,sso" => "settings-methods-local-sso",
    "sso" => "settings-methods-sso",
    "local" => "settings-methods-local",
    _ => "settings-methods-sso-local",
  }
}

/// The i18n key of a notification level, worded as in the user's own settings.
fn level_choice_key(choice: &str) -> &'static str {
  match choice {
    "mentions" => "notify-level-mentions",
    _ => "notify-level-all",
  }
}

/// The i18n key that names a policy for binding tokens to devices.
fn dpop_policy_choice_key(choice: &str) -> &'static str {
  match choice {
    "required" => "settings-dpop-required",
    "off" => "settings-dpop-off",
    _ => "settings-dpop-optional",
  }
}

/// How the holiday feed stands: its copy, or why the last fetch failed.
#[component]
fn HolidayFeedLine() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_status = Memo::new(move |_| {
    state
      .read(|backend| backend.runtime_settings())
      .and_then(|response| response.maybe_holiday_feed)
  });
  move || {
    maybe_status.get().map(|status| {
      let text = holiday_feed_text(&status, |key, args| i18n.t_with(key, args), i18n.locale().tag());
      let class = if status.maybe_error.is_some() {
        "field-error small"
      } else {
        "muted-text small"
      };
      view! { <p class=class role="status">{text}</p> }
    })
  }
}

/// The sentence of [`HolidayFeedLine`]; `t` translates a key with arguments.
fn holiday_feed_text(
  status: &HolidayFeedStatus,
  t: impl Fn(&str, &[(&str, &str)]) -> String,
  locale: &str,
) -> String {
  let now = now_ms();
  let maybe_copy = status.maybe_fetched_at.map(|at| {
    t(
      "settings-holiday-feed-copy",
      &[
        ("count", &status.holidays.to_string()),
        ("when", &format_relative(at.0, now, locale)),
      ],
    )
  });
  match &status.maybe_error {
    None => maybe_copy.unwrap_or_else(|| t("settings-holiday-feed-waiting", &[])),
    Some(error) => {
      let reason = match error {
        HolidayFeedError::HttpStatus => t(
          "settings-holiday-feed-error-http_status",
          &[(
            "status",
            &status
              .maybe_http_status
              .map(|code| code.to_string())
              .unwrap_or_default(),
          )],
        ),
        HolidayFeedError::Unknown(_) => t("settings-holiday-feed-error-unreachable", &[]),
        known => t(&format!("settings-holiday-feed-error-{}", known.as_str()), &[]),
      };
      let kept = maybe_copy.unwrap_or_else(|| t("settings-holiday-feed-no-copy", &[]));
      t(
        "settings-holiday-feed-failed",
        &[("reason", &reason), ("kept", &kept)],
      )
    },
  }
}

/// The i18n key or DOM id of a field, which components take as `&'static str`. The set is the
/// fixed field tables above, so the few bytes leaked per field row are bounded.
fn leak_key(text: String) -> &'static str {
  Box::leak(text.into_boxed_str())
}

/// The DOM id of a group's form, where a refused save looks for the first invalid field.
fn form_id(slug: &str) -> String {
  format!("settings-form-{slug}")
}

fn fields_of(slug: &str) -> &'static [FieldDef] {
  match slug {
    "server_rules" => SERVER_RULES,
    "notifications" => NOTIFICATIONS,
    "email_digest" => EMAIL_DIGEST,
    "shares" => SHARES,
    "limits" => LIMITS,
    "integration" => INTEGRATION,
    "local_accounts" => LOCAL_ACCOUNTS,
    "sign_in_page" => SIGN_IN_PAGE,
    "mail_texts" => MAIL_TEXTS,
    "meeting_mail" => MEETING_MAIL,
    "token_binding" => TOKEN_BINDING,
    "app_links" => APP_LINKS,
    "health_alerts" => HEALTH_ALERTS,
    "holidays" => HOLIDAYS,
    "crash_reports" => CRASH_REPORTS,
    _ => &[],
  }
}

/// The settings of one group as the JSON object the wire uses.
fn group_json(key: &RuntimeSettingsKey, response: &RuntimeSettingsResponse) -> Option<Value> {
  let value = match key {
    RuntimeSettingsKey::ServerRules => serde_json::to_value(&response.server_rules),
    RuntimeSettingsKey::Notifications => serde_json::to_value(&response.notifications),
    RuntimeSettingsKey::EmailDigest => serde_json::to_value(&response.email_digest),
    RuntimeSettingsKey::Shares => serde_json::to_value(&response.shares),
    RuntimeSettingsKey::Limits => serde_json::to_value(&response.limits),
    RuntimeSettingsKey::Integration => serde_json::to_value(&response.integration),
    RuntimeSettingsKey::LocalAccounts => serde_json::to_value(&response.local_accounts),
    RuntimeSettingsKey::SignInPage => serde_json::to_value(&response.sign_in_page),
    RuntimeSettingsKey::MailTexts => serde_json::to_value(&response.mail_texts),
    RuntimeSettingsKey::MeetingMail => serde_json::to_value(&response.meeting_mail),
    RuntimeSettingsKey::TokenBinding => serde_json::to_value(&response.token_binding),
    RuntimeSettingsKey::AppLinks => serde_json::to_value(&response.app_links),
    RuntimeSettingsKey::HealthAlerts => serde_json::to_value(&response.health_alerts),
    RuntimeSettingsKey::Holidays => serde_json::to_value(&response.holidays),
    RuntimeSettingsKey::CrashReports => serde_json::to_value(&response.crash_reports),
    _ => return None,
  };
  value.ok()
}

/// Form values for a group, from the settings in force.
fn to_values(key: &RuntimeSettingsKey, response: &RuntimeSettingsResponse) -> Values {
  let json = group_json(key, response).unwrap_or(Value::Null);
  fields_of(key.as_str())
    .iter()
    .map(|field| (field.id, value_text(field.kind, lookup(&json, field.id))))
    .collect()
}

/// A field's value in the group's JSON; `map.key` reaches into an object.
fn lookup<'a>(json: &'a Value, id: &str) -> Option<&'a Value> {
  match id.split_once('.') {
    Some((map, key)) => json.get(map)?.get(key),
    None => json.get(id),
  }
}

fn value_text(kind: Kind, maybe_value: Option<&Value>) -> String {
  let Some(value) = maybe_value else {
    return String::new();
  };
  match kind {
    Kind::Bool => value.as_bool().unwrap_or(false).to_string(),
    Kind::Number { scale, .. } => value
      .as_u64()
      .map(|raw| format_scaled(raw, scale))
      .unwrap_or_default(),
    Kind::Text
    | Kind::Dsn
    | Kind::Environment
    | Kind::Area
    | Kind::MapText
    | Kind::Link
    | Kind::BridgeUrl
    | Kind::Level
    | Kind::DpopPolicy
    | Kind::CrashMode
    | Kind::HolidayRegion => value.as_str().unwrap_or_default().to_owned(),
    Kind::Methods => value
      .as_array()
      .map(|methods| {
        methods
          .iter()
          .filter_map(Value::as_str)
          .collect::<Vec<_>>()
          .join(",")
      })
      .unwrap_or_default(),
    Kind::IdList | Kind::TextList => {
      let items: Vec<String> = value
        .as_array()
        .map(|items| {
          items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
        })
        .unwrap_or_default();
      list_text(&items)
    },
  }
}

/// The section to send for the typed values, or what is wrong with them. A problem that belongs to
/// no single field is reported under `form`.
fn check(key: &RuntimeSettingsKey, values: &Values) -> Result<RuntimeSettingsSection, FieldErrors> {
  let mut errors = FieldErrors::new();
  let mut object = Map::new();
  for field in fields_of(key.as_str()) {
    let typed = values.get(field.id).map(String::as_str).unwrap_or_default();
    match field_json(field.kind, typed) {
      // Empty optional values are left out (none).
      Ok(Value::Null) => {},
      Ok(value) => match field.id.split_once('.') {
        Some((map, key)) => {
          if let Some(entries) = object
            .entry(map.to_owned())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
          {
            entries.insert(key.to_owned(), value);
          }
        },
        None => {
          object.insert(field.id.to_owned(), value);
        },
      },
      Err(message) => errors.push((field.id, message)),
    }
  }
  // An own backend needs its DSN.
  if *key == RuntimeSettingsKey::CrashReports
    && object.get("mode").and_then(Value::as_str) == Some("own")
    && object.get("dsn").and_then(Value::as_str) == Some("")
  {
    // The DSN field comes right after the switch, before any other field that can be wrong.
    errors.insert(0, ("dsn", "settings-error-dsn"));
  }
  if !errors.is_empty() {
    return Err(errors);
  }
  serde_json::from_value(json!({ "section": key.as_str(), "values": object }))
    .map_err(|_| vec![("form", "admin-error-invalid")])
}

fn field_json(kind: Kind, typed: &str) -> Result<Value, &'static str> {
  match kind {
    Kind::Bool => Ok(Value::Bool(typed == "true")),
    Kind::Number {
      is_optional: true, ..
    } if typed.trim().is_empty() => Ok(Value::Null),
    Kind::Number {
      scale,
      min,
      max,
      maybe_range_error,
      ..
    } => match parse_scaled(typed, scale) {
      Some(raw) if raw >= min && raw <= max => Ok(json!(raw)),
      _ => Err(maybe_range_error.unwrap_or("admin-error-number")),
    },
    Kind::Text | Kind::Area => Ok(Value::String(typed.trim_end().to_owned())),
    Kind::Dsn => match typed.trim() {
      "" => Ok(json!("")),
      dsn if dsn.parse::<Dsn>().is_ok() => Ok(json!(dsn)),
      _ => Err("settings-error-dsn"),
    },
    Kind::Environment => match typed.trim() {
      name if is_crash_environment(name) => Ok(json!(name)),
      _ => Err("settings-error-crash-environment"),
    },
    Kind::MapText => Ok(match typed.trim() {
      "" => Value::Null,
      text => Value::String(text.to_owned()),
    }),
    Kind::Link => match typed.trim() {
      "" => Ok(Value::Null),
      link if link.starts_with("https://") && is_web_url(link) => Ok(Value::String(link.to_owned())),
      _ => Err("admin-error-url"),
    },
    Kind::BridgeUrl => match typed.trim() {
      "" => Ok(Value::Null),
      url => check_bridge_public_url(url)
        .map(Value::String)
        .map_err(|_| "settings-error-bridge-url"),
    },
    Kind::Methods => {
      let choice = METHOD_CHOICES
        .into_iter()
        .find(|choice| *choice == typed)
        .unwrap_or(METHOD_CHOICES[0]);
      Ok(json!(choice.split(',').collect::<Vec<_>>()))
    },
    Kind::Level => {
      let choice = LEVEL_CHOICES
        .into_iter()
        .find(|choice| *choice == typed)
        .unwrap_or(LEVEL_CHOICES[0]);
      Ok(json!(choice))
    },
    Kind::CrashMode => Ok(json!(
      CRASH_MODE_CHOICES
        .into_iter()
        .find(|choice| *choice == typed)
        .unwrap_or(CRASH_MODE_CHOICES[0])
    )),
    Kind::DpopPolicy => {
      let choice = DPOP_POLICY_CHOICES
        .into_iter()
        .find(|choice| *choice == typed)
        .unwrap_or(DPOP_POLICY_CHOICES[0]);
      Ok(json!(choice))
    },
    Kind::HolidayRegion => Ok(
      HOLIDAY_REGIONS
        .into_iter()
        .find(|code| *code == typed)
        .map_or(Value::Null, |code| json!(code)),
    ),
    Kind::TextList => Ok(json!(parse_list(typed))),
    Kind::IdList => {
      let items = parse_list(typed);
      if items.iter().all(|item| item.parse::<Snowflake>().is_ok()) {
        Ok(json!(items))
      } else {
        Err("admin-error-ids")
      }
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn kind(slug: &str) -> RuntimeSettingsKey {
    RuntimeSettingsKey::from(slug.to_owned())
  }

  fn typed(pairs: &[(&'static str, &str)]) -> Values {
    pairs.iter().map(|(id, text)| (*id, (*text).to_owned())).collect()
  }

  #[test]
  fn every_group_has_fields_and_every_key_is_known() {
    for slug in GROUPS {
      assert!(!fields_of(slug).is_empty(), "{slug}");
      assert!(
        !matches!(kind(slug), RuntimeSettingsKey::Unknown(_)),
        "{slug} is not a settings key"
      );
    }
  }

  #[test]
  fn a_section_is_built_from_typed_values_in_the_shown_units() {
    let values = typed(&[
      ("is_enabled", "true"),
      ("default_interval_secs", "30"),
      ("min_interval_secs", "1.5"),
      ("grace_secs", "10"),
      ("subject_one", "One mention"),
      ("subject_many", "{count} mentions"),
      ("body_one", "Open {url}"),
      ("body_many", "Open {url}"),
    ]);
    let Ok(RuntimeSettingsSection::EmailDigest(digest)) = check(&kind("email_digest"), &values) else {
      panic!("the digest should be valid");
    };
    assert!(digest.is_enabled);
    assert_eq!(digest.default_interval_secs, 1_800);
    assert_eq!(digest.min_interval_secs, 90);
    assert_eq!(digest.grace_secs, 600);
  }

  #[test]
  fn lists_and_numbers_report_their_field() {
    let values = typed(&[
      ("is_enabled", "true"),
      ("default_expiry_days", "-3"),
      ("max_expiry_days", "30"),
      ("max_downloads_cap", "0"),
      ("max_links_per_user", "5"),
    ]);
    assert_eq!(
      check(&kind("shares"), &values).err(),
      Some(vec![("default_expiry_days", "admin-error-number")])
    );
    let ids = typed(&[
      ("invites_require_verified_email", "true"),
      ("default_spaces", "12, 13"),
      ("default_channels", "abc"),
      ("admin_join_request_days", "7"),
    ]);
    assert_eq!(
      check(&kind("server_rules"), &ids).err(),
      Some(vec![("default_channels", "admin-error-ids")])
    );
  }

  #[test]
  fn ids_pass_as_strings() {
    let rules = typed(&[
      ("invites_require_verified_email", "true"),
      ("default_spaces", "12\n13"),
      ("default_channels", ""),
      ("admin_join_request_days", "7"),
    ]);
    let Ok(RuntimeSettingsSection::ServerRules(rules)) = check(&kind("server_rules"), &rules) else {
      panic!("valid");
    };
    assert_eq!(rules.default_spaces.len(), 2);
    assert!(rules.default_channels.is_empty());
  }

  #[test]
  fn the_removal_period_is_optional() {
    let empty = typed(&[
      ("invites_require_verified_email", "true"),
      ("remove_deactivated_after_days", ""),
      ("admin_join_request_days", "7"),
    ]);
    let Ok(RuntimeSettingsSection::ServerRules(rules)) = check(&kind("server_rules"), &empty) else {
      panic!("valid");
    };
    assert_eq!(rules.maybe_remove_deactivated_after_days, None);
    let set = typed(&[
      ("invites_require_verified_email", "true"),
      ("remove_deactivated_after_days", "90"),
      ("admin_join_request_days", "7"),
    ]);
    let Ok(RuntimeSettingsSection::ServerRules(rules)) = check(&kind("server_rules"), &set) else {
      panic!("valid");
    };
    assert_eq!(rules.maybe_remove_deactivated_after_days, Some(90));
    let wrong = typed(&[
      ("invites_require_verified_email", "true"),
      ("remove_deactivated_after_days", "-1"),
      ("admin_join_request_days", "7"),
    ]);
    assert_eq!(
      check(&kind("server_rules"), &wrong).err(),
      Some(vec![("remove_deactivated_after_days", "admin-error-number")])
    );
  }

  /// Issue #776: how long admins' join requests wait, in days; always set.
  #[test]
  fn the_join_request_expiry_is_a_number_of_days() {
    let set = typed(&[
      ("invites_require_verified_email", "true"),
      ("admin_join_request_days", "14"),
    ]);
    let Ok(RuntimeSettingsSection::ServerRules(rules)) = check(&kind("server_rules"), &set) else {
      panic!("valid");
    };
    assert_eq!(rules.admin_join_request_days, 14);
    let empty = typed(&[
      ("invites_require_verified_email", "true"),
      ("admin_join_request_days", ""),
    ]);
    assert!(check(&kind("server_rules"), &empty).is_err());
  }

  #[test]
  fn the_sign_in_page_maps_methods_help_texts_and_the_link() {
    let values = typed(&[
      ("methods", "local,sso"),
      ("help_text.en", "Call 1234."),
      ("help_text.de", "  "),
      ("help_link", ""),
    ]);
    let Ok(RuntimeSettingsSection::SignInPage(page)) = check(&kind("sign_in_page"), &values) else {
      panic!("valid");
    };
    assert_eq!(
      page.methods,
      [
        teamcraft_proto::SignInMethod::Local,
        teamcraft_proto::SignInMethod::Sso
      ]
    );
    assert_eq!(page.help_text.len(), 1);
    assert_eq!(page.help_text["en"], "Call 1234.");
    assert!(page.maybe_help_link.is_none());

    let response_json =
      json!({"methods": ["sso"], "help_text": {"de": "Hilfe"}, "help_link": "https://help.example.com"});
    assert_eq!(
      value_text(Kind::Methods, lookup(&response_json, "methods")),
      "sso"
    );
    assert_eq!(
      value_text(Kind::MapText, lookup(&response_json, "help_text.de")),
      "Hilfe"
    );
    assert_eq!(
      value_text(Kind::MapText, lookup(&response_json, "help_text.en")),
      ""
    );

    let bad_link = typed(&[("methods", "sso"), ("help_link", "http://help.example.com")]);
    assert_eq!(
      check(&kind("sign_in_page"), &bad_link).err(),
      Some(vec![("help_link", "admin-error-url")])
    );
  }

  #[test]
  fn too_large_counts_are_refused_before_sending() {
    let values = typed(&[("default_retention_days", "99999999999")]);
    assert_eq!(
      check(&kind("integration"), &values).err(),
      Some(vec![("default_retention_days", "admin-error-number")])
    );
  }

  #[test]
  fn the_bridge_public_url_is_none_or_a_checked_address() {
    let sent = |text: &str| {
      let values = typed(&[("default_retention_days", "30"), ("bridge_public_url", text)]);
      match check(&kind("integration"), &values) {
        Ok(RuntimeSettingsSection::Integration(values)) => Ok(values.maybe_bridge_public_url),
        Ok(_) => panic!("wrong section"),
        Err(errors) => Err(errors),
      }
    };
    assert_eq!(sent(""), Ok(None));
    assert_eq!(sent("  "), Ok(None));
    assert_eq!(
      sent(" https://bridge.example.com/ "),
      Ok(Some("https://bridge.example.com".to_owned()))
    );
    assert_eq!(
      sent("http://127.0.0.1:8088"),
      Ok(Some("http://127.0.0.1:8088".to_owned()))
    );
    for bad in [
      "http://bridge.example.com",
      "https://user:pw@bridge.example.com",
      "https://bridge.example.com/#x",
      "https://bridge.example.com/?a=1",
      "bridge.example.com",
    ] {
      assert_eq!(
        sent(bad),
        Err(vec![("bridge_public_url", "settings-error-bridge-url")]),
        "{bad}"
      );
    }
    let response_json =
      json!({"default_retention_days": 30, "bridge_public_url": "https://bridge.example.com"});
    assert_eq!(
      value_text(Kind::BridgeUrl, lookup(&response_json, "bridge_public_url")),
      "https://bridge.example.com"
    );
    assert_eq!(
      value_text(Kind::BridgeUrl, lookup(&json!({}), "bridge_public_url")),
      ""
    );
  }

  #[test]
  fn crash_reports_need_a_valid_dsn_while_on() {
    let mut values: Values = [
      ("mode", "own"),
      ("dsn", ""),
      ("sample_percent", "150"),
      ("trace_percent", "101"),
      ("slow_job_ms", "1000"),
      ("call_reports_per_call", "8"),
      ("environment", "Prod"),
    ]
    .into_iter()
    .map(|(id, value)| (id, value.to_owned()))
    .collect();
    assert_eq!(
      check(&kind("crash_reports"), &values).err(),
      Some(vec![
        ("dsn", "settings-error-dsn"),
        ("sample_percent", "settings-error-crash-percent"),
        ("trace_percent", "settings-error-crash-percent"),
        ("environment", "settings-error-crash-environment"),
      ])
    );
    values.insert("dsn", "not a dsn".to_owned());
    values.insert("sample_percent", "10".to_owned());
    values.insert("trace_percent", "5".to_owned());
    values.insert("environment", "staging".to_owned());
    assert_eq!(
      check(&kind("crash_reports"), &values).err(),
      Some(vec![("dsn", "settings-error-dsn")])
    );
    values.insert("dsn", "https://key@errors.example.com/4".to_owned());
    assert!(check(&kind("crash_reports"), &values).is_ok());
    values.insert("allows_opt_out", "false".to_owned());
    values.insert("shows_notice", "false".to_owned());
    let Ok(RuntimeSettingsSection::CrashReports(mandatory)) = check(&kind("crash_reports"), &values) else {
      panic!("a valid section");
    };
    assert!(!mandatory.allows_opt_out && !mandatory.shows_notice);
    values.insert("mode", "publisher".to_owned());
    values.insert("dsn", String::new());
    assert!(check(&kind("crash_reports"), &values).is_ok());
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<String> = vec![
      "admin-settings-title".into(),
      "admin-settings-subtitle".into(),
      "admin-settings-loading".into(),
      "settings-list-hint".into(),
      "admin-save".into(),
      "admin-reset-to-config".into(),
      "admin-error-number".into(),
      "admin-error-ids".into(),
      "admin-error-invalid".into(),
    ];
    for unit in [
      SECONDS, MINUTES, HOURS, DAYS, MEGABYTES, KILOBYTES, PIXELS, PERCENT,
    ] {
      keys.push(unit.into());
    }
    for error in [
      "settings-error-health-minutes",
      "settings-error-health-backup-hours",
      "settings-error-health-days",
      "settings-error-health-percent",
      "settings-error-crash-percent",
      "settings-error-slow-job",
      "settings-unit-ms",
      "settings-error-dsn",
      "settings-error-crash-environment",
      "settings-crash-mode-publisher-none",
      "settings-crash-mode-own",
      "settings-crash-mode-off",
      "settings-error-holiday-refresh-hours",
      "settings-error-holiday-max-age-days",
      "settings-holiday-feed-copy",
      "settings-holiday-feed-waiting",
      "settings-holiday-feed-failed",
      "settings-holiday-feed-no-copy",
      "settings-holiday-feed-error-unreachable",
      "settings-holiday-feed-error-timeout",
      "settings-holiday-feed-error-http_status",
      "settings-holiday-feed-error-too_large",
      "settings-holiday-feed-error-not_a_calendar",
      "settings-holiday-feed-error-malformed",
      "settings-holiday-feed-error-empty",
      "settings-holiday-feed-error-too_many",
    ] {
      keys.push(error.into());
    }
    for code in std::iter::once("").chain(HOLIDAY_REGIONS) {
      keys.push(holiday_region_key(code));
    }
    for slug in GROUPS {
      keys.push(format!("settings-group-{slug}"));
      keys.push(format!("settings-group-{slug}-hint"));
      for field in fields_of(slug) {
        keys.push(label_key(field.id));
      }
    }
    for choice in METHOD_CHOICES {
      keys.push(method_choice_key(choice).into());
    }
    keys.push("admin-error-url".into());
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
