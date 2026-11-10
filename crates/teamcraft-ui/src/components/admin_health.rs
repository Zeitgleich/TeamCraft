use leptos::prelude::*;
use teamcraft_proto::{
  AdminHealthResponse, AdminOidcSyncResponse, DeactivationCause, HealthAlertResponse, HealthCheckId,
  HealthCheckResponse, HealthCheckStatus, OidcSyncError, OidcSyncMode, OidcSyncOutcome, OidcSyncState,
  PlannedDeactivationResponse,
};

use crate::admin::AdminSection;
use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::copy_button::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// How the server's health checks stand and the alerts raised (docs/operations.md, "Health and
/// alerts"). One small read when the page opens, one more when the server pushes a change, and one
/// every `REFRESH_MS` of the console while it stays open: the server's answer is its last
/// evaluation, so the page costs it nothing.
#[component]
pub fn HealthSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let health = Memo::new(move |_| state.read(|backend| backend.health_status()));

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-health-title"))
      subtitle=Signal::derive(move || i18n.t("admin-health-subtitle"))
    />
    {move || match health.get() {
      None => view! { <p class="empty-state" role="status">{move || i18n.t("health-loading")}</p> }.into_any(),
      Some(health) => view! { <HealthContent health=health /> }.into_any(),
    }}
    <OidcSyncCard />
    <ProviderEventsCard />
  }
}

/// The sync with the identity provider: how the last run went, overdue or not, and what its safety
/// brake holds back (docs/admin.md §3.2.3).
#[component]
fn OidcSyncCard() -> impl IntoView {
  let state = use_app_state();
  let sync = Memo::new(move |_| state.read(|backend| backend.oidc_sync_status()));
  let preview = Memo::new(move |_| state.read(|backend| backend.oidc_sync_preview()));
  view! {
    {move || sync.get().map(|sync| view! { <OidcSyncContent sync=sync preview=preview /> })}
  }
}

/// The identity provider's events (docs/provisioning.md §4): whether a secret is set, when the last
/// event came, a new secret shown once, and turning it off.
#[component]
fn ProviderEventsCard() -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let hook = Memo::new(move |_| state.read(|backend| backend.provider_event_hook()));
  let revealed = Memo::new(move |_| state.read(|backend| backend.revealed_provider_event_secret()));
  let is_confirming = RwSignal::new(false);
  let locale = move || i18n.locale().tag();
  view! {
    {move || hook.get().map(|hook| {
      let now = now_ms();
      let is_configured = hook.is_configured;
      let status = match (hook.is_configured, hook.maybe_last_event_at) {
        (false, _) => i18n.t("provider-events-off"),
        (true, None) => i18n.t("provider-events-none-yet"),
        (true, Some(at)) => i18n.t_with("provider-events-last", &[("when", &format_relative(at.0, now, locale()))]),
      };
      view! {
        <section class="instance-card" aria-labelledby="health-provider-events">
          <h3 id="health-provider-events"><Icon name=IconName::Activity /> {move || i18n.t("provider-events-title")}</h3>
          <p class="muted-text small">{move || i18n.t("provider-events-hint")}</p>
          {(!hook.is_sync_active).then(|| view! { <p class="muted-text">{move || i18n.t("provider-events-needs-sync")}</p> })}
          <p role="status">{status}</p>
          {move || revealed.get().map(|secret| {
            let text = secret.expose().to_owned();
            view! {
              <section class="integration-reveal" role="status" aria-live="polite">
                <p class="muted-text">{move || i18n.t("provider-events-secret-once")}</p>
                <label for="provider-event-secret">{move || i18n.t("admin-webhooks-secret")}</label>
                <div class="integration-copy-row">
                  <input id="provider-event-secret" class="input mono" readonly translate="no" prop:value=text.clone() />
                  <CopyButton text=text.clone() />
                  <span class="grow"></span>
                  <button type="button" class="btn" on:click=move |_| state.write(|backend| backend.dismiss_provider_event_secret())>
                    {move || i18n.t("admin-integrations-reveal-done")}
                  </button>
                </div>
              </section>
            }
          })}
          <p class="row-actions">
            <button type="button" class="btn btn-small" on:click=move |_| state.write(|backend| backend.create_provider_event_secret())>
              {move || if is_configured { i18n.t("provider-events-rotate") } else { i18n.t("provider-events-create") }}
            </button>
            {is_configured.then(|| view! {
              <Show
                when=move || is_confirming.get()
                fallback=move || view! {
                  <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(true)>
                    {move || i18n.t("provider-events-delete")}
                  </button>
                }
              >
                <span>{move || i18n.t("provider-events-delete-question")}</span>
                " "
                <button
                  type="button"
                  class="btn btn-small btn-danger"
                  on:click=move |_| {
                    state.write(|backend| backend.delete_provider_event_hook());
                    is_confirming.set(false);
                  }
                >
                  {move || i18n.t("provider-events-delete-yes")}
                </button>
                <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
                  {move || i18n.t("cancel")}
                </button>
              </Show>
            })}
          </p>
        </section>
      }
    })}
  }
}

#[component]
fn OidcSyncContent(
  sync: AdminOidcSyncResponse,
  preview: Memo<Option<teamcraft_proto::AdminOidcSyncPreviewResponse>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let is_confirming = RwSignal::new(false);
  let now = now_ms();
  let revision = sync.held_revision;
  let held = sync.held.clone();
  let held_count = held.len();
  let run = sync.maybe_last_run.clone();
  let is_stale = sync.is_stale;
  let stale_after = usize::try_from(sync.stale_after_intervals).unwrap_or(usize::MAX);
  let is_active = sync.state == OidcSyncState::Active;
  let is_dry_run = sync.mode == OidcSyncMode::DryRun;
  let not_running_key = match sync.state {
    OidcSyncState::SwitchedOff => Some("oidc-sync-switched-off"),
    OidcSyncState::Active => None,
    OidcSyncState::NotConfigured | OidcSyncState::Unknown(_) => Some("oidc-sync-not-configured"),
  };
  let pending_removals = usize::try_from(sync.pending_removals).unwrap_or(usize::MAX);
  let unfinished = usize::try_from(sync.unfinished_deactivations).unwrap_or(usize::MAX);
  let locale = move || i18n.locale().tag();
  view! {
    <section class="instance-card" aria-labelledby="health-oidc-sync">
      <h3 id="health-oidc-sync"><Icon name=IconName::Activity /> {move || i18n.t("oidc-sync-title")}</h3>
      {not_running_key
        .map(|key| {
          view! {
            <div class="health-summary health-summary-failing" role="status">
              <Icon name=IconName::Alert />
              <p>{move || i18n.t(key)}</p>
            </div>
          }
        })}
      {is_dry_run
        .then(|| {
          view! { <p class="muted-text">{move || i18n.t("oidc-sync-dry-run")}</p> }
        })}
      {is_stale
        .then(|| {
          view! {
            <div class="health-summary health-summary-alert" role="alert">
              <Icon name=IconName::Alert />
              <p>{move || i18n.t_count("oidc-sync-stale", stale_after)}</p>
            </div>
          }
        })}
      {(held_count > 0)
        .then(|| {
          view! {
            <div class="health-summary health-summary-alert" role="alert">
              <Icon name=IconName::Alert />
              <div>
                <strong>{move || i18n.t_count("oidc-sync-held", held_count)}</strong>
                <p class="muted-text small">{move || i18n.t("oidc-sync-held-hint")}</p>
                <ul class="oidc-sync-held">
                  {held
                    .into_iter()
                    .map(|entry| {
                      let key = cause_key(&entry.cause);
                      let name = entry.display_name.clone();
                      let username = entry.maybe_username.clone();
                      let is_confirmed = entry.is_confirmed;
                      view! {
                        <li>
                          <span translate="no">{name}</span>
                          {username.map(|username| view! { <span class="muted-text small" translate="no">" @" {username}</span> })}
                          " — "
                          <span class="muted-text small">{move || i18n.t(key)}</span>
                          {is_confirmed.then(|| view! { <span class="badge">{move || i18n.t("oidc-sync-confirmed")}</span> })}
                        </li>
                      }
                    })
                    .collect_view()}
                </ul>
                <Show
                  when=move || is_confirming.get()
                  fallback=move || {
                    view! {
                      <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(true)>
                        {move || i18n.t("oidc-sync-confirm")}
                      </button>
                    }
                  }
                >
                  <p>{move || i18n.t_count("oidc-sync-confirm-question", held_count)}</p>
                  <button
                    type="button"
                    class="btn btn-small btn-danger"
                    on:click=move |_| {
                      state.write(|backend| backend.confirm_oidc_sync_brake(revision));
                      is_confirming.set(false);
                    }
                  >
                    {move || i18n.t("oidc-sync-confirm-yes")}
                  </button>
                  <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
                    {move || i18n.t("cancel")}
                  </button>
                </Show>
              </div>
            </div>
          }
        })}
      {move || match run.clone() {
        None => view! { <p class="muted-text">{move || i18n.t("oidc-sync-never-ran")}</p> }.into_any(),
        Some(run) => {
          let outcome = outcome_key(&run.outcome);
          let maybe_error = run.maybe_error.as_ref().map(error_key);
          let failures = run.consecutive_failures;
          let finished = run.finished_at.0;
          let summary = i18n.t_with(
            "oidc-sync-changes",
            &[
              ("deactivated", &run.deactivated.to_string()),
              ("reactivated", &run.reactivated.to_string()),
              ("roles", &run.role_changes.to_string()),
              ("groups", &run.groups_changed.to_string()),
              ("failed", &run.failed.to_string()),
            ],
          );
          let duration = i18n.t_with(
            "oidc-sync-duration",
            &[
              ("when", &format_relative(finished, now, locale())),
              ("seconds", &(run.duration_ms.max(0) + 999).div_euclid(1000).to_string()),
            ],
          );
          view! {
            <dl class="oidc-sync-run">
              <dt>{move || i18n.t("oidc-sync-last-run")}</dt>
              <dd>{duration}</dd>
              <dt>{move || i18n.t("oidc-sync-outcome")}</dt>
              <dd>
                {move || i18n.t(outcome)}
                {maybe_error.map(|key| view! { " " <span>{move || i18n.t(key)}</span> })}
                {(failures > 1).then(|| view! { " " <span class="muted-text small">{move || i18n.t_count("oidc-sync-failures", usize::try_from(failures).unwrap_or(usize::MAX))}</span> })}
              </dd>
              <dt>{move || i18n.t("oidc-sync-changes-title")}</dt>
              <dd>{summary}</dd>
            </dl>
          }
            .into_any()
        },
      }}
      {(unfinished > 0).then(|| view! { <p class="muted-text small">{move || i18n.t_count("oidc-sync-unfinished", unfinished)}</p> })}
      {move || {
        let held = state.read(|backend| backend.leavers_held_by_limit());
        (held > 0).then(|| view! {
          <div class="health-summary health-summary-alert" role="status">
            <Icon name=IconName::Alert />
            <p>
              {move || i18n.t_count("oidc-sync-leavers-held", held)}
              " "
              <button type="button" class="btn btn-small" on:click=move |_| state.open_admin(AdminSection::Users)>
                {move || i18n.t("oidc-sync-open-users")}
              </button>
            </p>
          </div>
        })
      }}
      {(pending_removals > 0)
        .then(|| {
          view! {
            <p class="muted-text">
              {move || i18n.t_count("oidc-sync-pending-removals", pending_removals)}
              " "
              <button type="button" class="btn btn-small" on:click=move |_| state.open_admin(AdminSection::Users)>
                {move || i18n.t("oidc-sync-open-users")}
              </button>
            </p>
          }
        })}
      {is_active
        .then(|| {
          view! {
            <p class="row-actions">
              <button type="button" class="btn btn-small" on:click=move |_| state.write(|backend| backend.run_oidc_sync_preview())>
                {move || i18n.t("oidc-sync-preview")}
              </button>
            </p>
          }
        })}
      {move || preview.get().map(|preview| {
        let count = preview.would_deactivate.len();
        let held = preview.would_hold.len();
        let reactivate = preview.would_reactivate;
        let roles = preview.would_change_roles;
        let allowed = preview.allowed;
        let active = preview.active_users;
        let would = preview.would_deactivate.clone();
        let would_hold = preview.would_hold.clone();
        view! {
          <div class="oidc-sync-preview" role="status">
            <strong>{move || i18n.t("oidc-sync-preview-title")}</strong>
            <p class="muted-text small">
              {move || i18n.t_with("oidc-sync-preview-summary", &[
                ("deactivate", &count.to_string()),
                ("held", &held.to_string()),
                ("reactivate", &reactivate.to_string()),
                ("roles", &roles.to_string()),
                ("allowed", &allowed.to_string()),
                ("active", &active.to_string()),
              ])}
            </p>
            <ul class="oidc-sync-held">
              {planned_rows(would)}
              {planned_rows(would_hold)}
            </ul>
          </div>
        }
      })}
    </section>
  }
}

fn planned_rows(planned: Vec<PlannedDeactivationResponse>) -> impl IntoView {
  let i18n = use_i18n();
  planned
    .into_iter()
    .map(|entry| {
      let key = cause_key(&entry.cause);
      view! {
        <li>
          <span translate="no">{entry.display_name.clone()}</span>
          " — "
          <span class="muted-text small">{move || i18n.t(key)}</span>
        </li>
      }
    })
    .collect_view()
}

fn cause_key(cause: &DeactivationCause) -> &'static str {
  match cause {
    DeactivationCause::Removed => "oidc-sync-cause-removed",
    DeactivationCause::Disabled => "oidc-sync-cause-disabled",
    DeactivationCause::NoRole => "oidc-sync-cause-no-role",
    DeactivationCause::Unknown(_) => "oidc-sync-cause-other",
  }
}

fn outcome_key(outcome: &OidcSyncOutcome) -> &'static str {
  match outcome {
    OidcSyncOutcome::Ok => "oidc-sync-outcome-ok",
    OidcSyncOutcome::Partial => "oidc-sync-outcome-partial",
    OidcSyncOutcome::Held => "oidc-sync-outcome-held",
    OidcSyncOutcome::DryRun => "oidc-sync-outcome-dry-run",
    OidcSyncOutcome::Failed | OidcSyncOutcome::Unknown(_) => "oidc-sync-outcome-failed",
  }
}

fn error_key(error: &OidcSyncError) -> &'static str {
  match error {
    OidcSyncError::ProviderRefused => "oidc-sync-error-refused",
    OidcSyncError::ProviderInvalidAnswer => "oidc-sync-error-invalid",
    OidcSyncError::ProviderListedNoUsers => "oidc-sync-error-no-users",
    OidcSyncError::Database => "oidc-sync-error-database",
    OidcSyncError::ProviderUnreachable | OidcSyncError::Unknown(_) => "oidc-sync-error-unreachable",
  }
}

#[component]
fn HealthContent(health: AdminHealthResponse) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let now = now_ms();
  let open_alerts = health.open_alert_count();
  let failing = health
    .checks
    .iter()
    .filter(|check| check.status == HealthCheckStatus::Failing)
    .count();
  let is_alerting_enabled = health.is_alerting_enabled;
  let hold_minutes = usize::try_from(health.hold_secs / 60).unwrap_or(usize::MAX);
  let checked = health.maybe_checked_at;
  let checks = health.checks.clone();
  let alerts = health.alerts.clone();
  let (summary_class, summary_icon) = if open_alerts > 0 {
    ("health-summary health-summary-alert", IconName::Alert)
  } else if failing > 0 {
    ("health-summary health-summary-failing", IconName::Alert)
  } else {
    ("health-summary", IconName::Check)
  };

  view! {
    <div class=summary_class role="status">
      <Icon name=summary_icon />
      <div>
        <strong>
          {move || {
            if open_alerts > 0 {
              i18n.t_count("health-summary-alerts", open_alerts)
            } else if failing > 0 {
              i18n.t_count("health-summary-failing", failing)
            } else {
              i18n.t("health-summary-ok")
            }
          }}
        </strong>
        <p class="muted-text small">
          {move || {
            if is_alerting_enabled {
              i18n.t_count("health-alerting-on", hold_minutes)
            } else {
              i18n.t("health-alerting-off")
            }
          }}
          " "
          {(!is_alerting_enabled)
            .then(|| {
              view! {
                <button type="button" class="btn btn-small" on:click=move |_| state.open_admin(AdminSection::Settings)>
                  {move || i18n.t("health-open-settings")}
                </button>
              }
            })}
        </p>
      </div>
    </div>

    <section class="instance-card" aria-labelledby="health-checks">
      <h3 id="health-checks"><Icon name=IconName::Activity /> {move || i18n.t("health-checks")}</h3>
      <ul class="health-checks">
        {checks
          .into_iter()
          .map(|check| view! { <CheckRow check=check now=now /> })
          .collect_view()}
      </ul>
      <p class="muted-text small">
        {move || match checked {
          Some(at) => i18n.t_with("health-checked", &[("when", &format_relative(at.0, now, i18n.locale().tag()))]),
          None => i18n.t("health-not-checked"),
        }}
        " "
        {move || i18n.t("health-footnote")}
      </p>
    </section>

    <section class="instance-card" aria-labelledby="health-alerts">
      <h3 id="health-alerts"><Icon name=IconName::Bell /> {move || i18n.t("health-alerts")}</h3>
      {if alerts.is_empty() {
        view! { <p class="muted-text">{move || i18n.t("health-alerts-none")}</p> }.into_any()
      } else {
        view! {
          <ul class="health-alert-list">
            {alerts
              .into_iter()
              .map(|alert| view! { <AlertRow alert=alert now=now /> })
              .collect_view()}
          </ul>
        }
          .into_any()
      }}
      <p class="muted-text small">{move || i18n.t("health-alerts-hint")}</p>
    </section>
  }
}

#[component]
fn CheckRow(check: HealthCheckResponse, now: i64) -> impl IntoView {
  let i18n = use_i18n();
  let slug = status_slug(&check.status);
  let id = check_key(&check.check);
  let detail = check_detail(&check);
  let failing_since = check.maybe_failing_since;
  let name_key = format!("health-check-{id}");
  let hint_key = format!("health-check-{id}-hint");
  let is_unknown_check = matches!(check.check, HealthCheckId::Unknown(_));
  let raw_name = check.check.as_str().to_owned();
  let aria_invalid = matches!(check.status, HealthCheckStatus::Alert);
  let icon = match check.status {
    HealthCheckStatus::Alert | HealthCheckStatus::Failing => IconName::Alert,
    HealthCheckStatus::Ok => IconName::Check,
    _ => IconName::Info,
  };

  view! {
    <li class=format!("health-check health-check-{slug}") data-alert=aria_invalid.then_some("true")>
      <div class="health-check-text">
        <strong>
          {move || if is_unknown_check { raw_name.clone() } else { i18n.t(&name_key) }}
        </strong>
        <span class="muted-text small">{move || if is_unknown_check { String::new() } else { i18n.t(&hint_key) }}</span>
        <span class="small">
          {move || detail.as_ref().map(|detail| detail.text(i18n, now))}
          " "
          {move || {
            failing_since
              .map(|since| {
                i18n.t_with(
                  "health-failing-since",
                  &[("when", &format_relative(since.0, now, i18n.locale().tag()))],
                )
              })
          }}
        </span>
      </div>
      <span class=format!("badge badge-health-{slug}")>
        <Icon name=icon />
        {move || i18n.t(&format!("health-status-{slug}"))}
      </span>
    </li>
  }
}

#[component]
fn AlertRow(alert: HealthAlertResponse, now: i64) -> impl IntoView {
  let i18n = use_i18n();
  let name_key = format!("health-check-{}", check_key(&alert.check));
  let is_unknown_check = matches!(alert.check, HealthCheckId::Unknown(_));
  let raw_name = alert.check.as_str().to_owned();
  let raised = alert.raised_at.0;
  let maybe_resolved = alert.maybe_resolved_at.map(|at| at.0);

  view! {
    <li>
      <strong>{move || if is_unknown_check { raw_name.clone() } else { i18n.t(&name_key) }}</strong>
      " "
      <span class="muted-text small">
        {move || {
          let locale = i18n.locale().tag();
          let raised = format_relative(raised, now, locale);
          match maybe_resolved {
            Some(at) => {
              i18n.t_with("health-alert-resolved", &[("raised", &raised), ("resolved", &format_relative(at, now, locale))])
            },
            None => i18n.t_with("health-alert-open", &[("raised", &raised)]),
          }
        }}
      </span>
    </li>
  }
}

/// What a check measured, to be worded in the reader's language.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Detail {
  /// Seconds since something last happened (a backup, a job run, the media server's report).
  Age {
    key: &'static str,
    secs: i64,
  },
  Days(i64),
  Percent(i64),
  /// Milliseconds the last probe took.
  Probe(i64),
  /// The 95th percentile of the calls' loss, in tenths of a percent.
  Loss(i64),
  /// Deactivations waiting for a decision.
  Held(usize),
  /// A job that has not run since the server started.
  NeverRan,
}

impl Detail {
  fn text(&self, i18n: crate::i18n::I18n, now: i64) -> String {
    match self {
      Self::Age { key, secs } => {
        let when = format_relative(now - secs * 1000, now, i18n.locale().tag());
        i18n.t_with(key, &[("when", &when)])
      },
      Self::Days(days) if *days < 0 => i18n.t("health-detail-certificate-expired"),
      Self::Days(days) => i18n.t_count("health-detail-certificate", usize::try_from(*days).unwrap_or(0)),
      Self::Percent(percent) => i18n.t_with("health-detail-disk", &[("percent", &percent.to_string())]),
      Self::Probe(millis) => i18n.t_with("health-detail-probe", &[("ms", &millis.to_string())]),
      Self::Loss(permille) => i18n.t_with("health-detail-loss", &[("percent", &tenths(*permille))]),
      Self::Held(count) => i18n.t_count("health-detail-held", *count),
      Self::NeverRan => i18n.t("health-detail-job-not-yet"),
    }
  }
}

/// The measured number of a check as a sentence, where it has one.
fn check_detail(check: &HealthCheckResponse) -> Option<Detail> {
  let is_job = matches!(
    check.check,
    HealthCheckId::OidcSync | HealthCheckId::Retention | HealthCheckId::PartitionMaintenance
  );
  match check.status {
    HealthCheckStatus::NotConfigured => None,
    // A job without data has not run since the server started.
    HealthCheckStatus::NoData | HealthCheckStatus::Unknown(_) => is_job.then_some(Detail::NeverRan),
    HealthCheckStatus::Ok | HealthCheckStatus::Failing | HealthCheckStatus::Alert => {
      measured(&check.check, check.maybe_value?)
    },
  }
}

fn measured(check: &HealthCheckId, value: i64) -> Option<Detail> {
  match check {
    HealthCheckId::Backup => Some(Detail::Age {
      key: "health-detail-backup",
      secs: value,
    }),
    HealthCheckId::MediaServer => Some(Detail::Age {
      key: "health-detail-report",
      secs: value,
    }),
    HealthCheckId::OidcSync
    | HealthCheckId::Retention
    | HealthCheckId::PartitionMaintenance
    | HealthCheckId::HolidayResync => Some(Detail::Age {
      key: "health-detail-job",
      secs: value,
    }),
    HealthCheckId::Certificate | HealthCheckId::SecretExpiry => Some(Detail::Days(value)),
    HealthCheckId::DiskSpace => Some(Detail::Percent(value)),
    HealthCheckId::ObjectStorage
    | HealthCheckId::PushGateway
    | HealthCheckId::MediaUdpIpv4
    | HealthCheckId::MediaUdpIpv6 => Some(Detail::Probe(value)),
    HealthCheckId::CallQuality => Some(Detail::Loss(value)),
    HealthCheckId::OidcSyncBrake if value > 0 => {
      Some(Detail::Held(usize::try_from(value).unwrap_or(usize::MAX)))
    },
    HealthCheckId::HolidayFeed => Some(Detail::Age {
      key: "health-detail-holiday-feed",
      secs: value,
    }),
    _ => None,
  }
}

/// Tenths as a decimal number with one place: 51 is "5.1". Plain digits in every language, as the
/// other details' numbers.
fn tenths(value: i64) -> String {
  format!("{}.{}", value.div_euclid(10), value.rem_euclid(10))
}

/// The part of the i18n keys that names a check.
fn check_key(check: &HealthCheckId) -> &str {
  check.as_str()
}

fn status_slug(status: &HealthCheckStatus) -> &'static str {
  match status {
    HealthCheckStatus::Ok => "ok",
    HealthCheckStatus::Failing => "failing",
    HealthCheckStatus::Alert => "alert",
    HealthCheckStatus::NotConfigured => "not-configured",
    HealthCheckStatus::NoData | HealthCheckStatus::Unknown(_) => "no-data",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn check(check: HealthCheckId, status: HealthCheckStatus, value: Option<i64>) -> HealthCheckResponse {
    HealthCheckResponse {
      check,
      status,
      maybe_failing_since: None,
      maybe_value: value,
    }
  }

  #[test]
  fn a_check_with_a_number_says_it_in_its_own_terms() {
    assert_eq!(
      check_detail(&check(HealthCheckId::Backup, HealthCheckStatus::Ok, Some(3600))),
      Some(Detail::Age {
        key: "health-detail-backup",
        secs: 3600
      })
    );
    assert_eq!(
      check_detail(&check(
        HealthCheckId::Certificate,
        HealthCheckStatus::Alert,
        Some(9)
      )),
      Some(Detail::Days(9))
    );
    assert_eq!(
      check_detail(&check(HealthCheckId::DiskSpace, HealthCheckStatus::Ok, Some(40))),
      Some(Detail::Percent(40))
    );
    assert_eq!(
      check_detail(&check(HealthCheckId::Database, HealthCheckStatus::Ok, None)),
      None
    );
    assert_eq!(
      check_detail(&check(
        HealthCheckId::MediaUdpIpv6,
        HealthCheckStatus::Ok,
        Some(14)
      )),
      Some(Detail::Probe(14))
    );
    assert_eq!(
      check_detail(&check(
        HealthCheckId::CallQuality,
        HealthCheckStatus::Alert,
        Some(73)
      )),
      Some(Detail::Loss(73))
    );
    assert_eq!(tenths(73), "7.3");
    assert_eq!(tenths(5), "0.5");
  }

  #[test]
  fn what_is_not_set_up_shows_no_number_and_a_job_without_a_run_says_so() {
    assert_eq!(
      check_detail(&check(
        HealthCheckId::Backup,
        HealthCheckStatus::NotConfigured,
        None
      )),
      None
    );
    assert_eq!(
      check_detail(&check(HealthCheckId::Retention, HealthCheckStatus::NoData, None)),
      Some(Detail::NeverRan)
    );
    assert_eq!(
      check_detail(&check(HealthCheckId::Backup, HealthCheckStatus::NoData, None)),
      None
    );
  }

  #[test]
  fn every_check_and_status_the_server_knows_has_a_label_and_a_slug() {
    for id in [
      HealthCheckId::Database,
      HealthCheckId::ObjectStorage,
      HealthCheckId::IdentityProvider,
      HealthCheckId::PushGateway,
      HealthCheckId::PushService,
      HealthCheckId::MediaServer,
      HealthCheckId::OidcSync,
      HealthCheckId::OidcSyncBrake,
      HealthCheckId::Retention,
      HealthCheckId::PartitionMaintenance,
      HealthCheckId::Backup,
      HealthCheckId::Certificate,
      HealthCheckId::DiskSpace,
      HealthCheckId::HolidayFeed,
      HealthCheckId::HolidayResync,
    ] {
      assert!(!check_key(&id).is_empty());
    }
    assert_eq!(
      status_slug(&HealthCheckStatus::Unknown("later".into())),
      "no-data"
    );
  }
}
