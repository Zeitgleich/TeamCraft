use leptos::prelude::*;
use teamcraft_proto::{
  AdminHealthResponse, HealthAlertResponse, HealthCheckId, HealthCheckResponse, HealthCheckStatus,
};

use crate::admin::AdminSection;
use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
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
  }
}

#[component]
fn HealthContent(health: AdminHealthResponse) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let now = now_ms();
  let open_alerts = health.open_alert_count();
  let is_alerting_enabled = health.is_alerting_enabled;
  let hold_minutes = usize::try_from(health.hold_secs / 60).unwrap_or(usize::MAX);
  let checked = health.maybe_checked_at;
  let checks = health.checks.clone();
  let alerts = health.alerts.clone();
  let (summary_class, summary_icon) = if open_alerts > 0 {
    ("health-summary health-summary-alert", IconName::Alert)
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
    HealthCheckId::OidcSync | HealthCheckId::Retention | HealthCheckId::PartitionMaintenance => {
      Some(Detail::Age {
        key: "health-detail-job",
        secs: value,
      })
    },
    HealthCheckId::Certificate => Some(Detail::Days(value)),
    HealthCheckId::DiskSpace => Some(Detail::Percent(value)),
    _ => None,
  }
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
      HealthCheckId::Retention,
      HealthCheckId::PartitionMaintenance,
      HealthCheckId::Backup,
      HealthCheckId::Certificate,
      HealthCheckId::DiskSpace,
    ] {
      assert!(!check_key(&id).is_empty());
    }
    assert_eq!(
      status_slug(&HealthCheckStatus::Unknown("later".into())),
      "no-data"
    );
  }
}
