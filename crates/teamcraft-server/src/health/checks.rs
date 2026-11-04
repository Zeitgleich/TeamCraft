//! What each check makes of the facts the server has: breaker states, push results, when jobs last
//! ran, what the deployment reported. Pure; [`super::HealthMonitor`] collects the facts.

use teamcraft_proto::{
  DependencyHealth, DependencyName, DependencyState, HealthAlertSettings, HealthCheckId, UnixMillis,
};
use time::{Duration, OffsetDateTime};

use super::heartbeat::{Heartbeats, Reading};
use super::tracker::{Observation, Verdict};

/// A job is late once it missed this many of its runs.
const JOB_MISSED_RUNS: i32 = 3;
/// The media server reports every few seconds; this long without a report is silence.
const SFU_SILENT_AFTER: Duration = Duration::seconds(120);
const SECONDS_PER_DAY: i64 = 86_400;

/// Every check, in the order the console shows them.
pub const ALL_CHECKS: [HealthCheckId; 12] = [
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
];

/// When a background job is expected to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobSchedule {
  pub interval: Duration,
  /// Time from the start of the server to the first run.
  pub initial_delay: Duration,
}

/// Which jobs run on this server and how often (they depend on the configuration).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobSchedules {
  pub maybe_oidc_sync: Option<JobSchedule>,
  pub maybe_retention: Option<JobSchedule>,
  pub maybe_partition_maintenance: Option<JobSchedule>,
}

/// The media server as the server sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaServerFacts {
  /// No media server, or it does not report to this server: nothing to judge by.
  NotMonitored,
  Reporting {
    maybe_last_contact: Option<UnixMillis>,
  },
}

/// A direct push service (not the gateway) that is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushFacts {
  pub is_configured: bool,
  pub is_failing: bool,
}

/// Everything one round of checks looks at.
pub struct Facts {
  pub now: OffsetDateTime,
  pub started_at: OffsetDateTime,
  /// How long after the start checks that need history give no verdict.
  pub startup_grace: Duration,
  pub settings: HealthAlertSettings,
  pub dependencies: Vec<DependencyHealth>,
  pub has_object_storage: bool,
  pub push: Vec<PushFacts>,
  pub media: MediaServerFacts,
  pub schedules: JobSchedules,
  pub oidc_sync_last_success: Option<OffsetDateTime>,
  pub retention_last_success: Option<OffsetDateTime>,
  pub partition_last_success: Option<OffsetDateTime>,
  pub heartbeats: Heartbeats,
}

/// The verdict of every check.
pub fn observe(facts: &Facts) -> Vec<(HealthCheckId, Observation)> {
  vec![
    (
      HealthCheckId::Database,
      dependency(facts, &DependencyName::Database, Absent::Healthy),
    ),
    (
      HealthCheckId::ObjectStorage,
      if facts.has_object_storage {
        dependency(facts, &DependencyName::ObjectStorage, Absent::Healthy)
      } else {
        Observation::new(Verdict::NotConfigured)
      },
    ),
    (
      HealthCheckId::IdentityProvider,
      dependency(facts, &DependencyName::IdentityProvider, Absent::Healthy),
    ),
    (
      HealthCheckId::PushGateway,
      dependency(facts, &DependencyName::PushGateway, Absent::NotConfigured),
    ),
    (HealthCheckId::PushService, push(&facts.push)),
    (HealthCheckId::MediaServer, media(facts)),
    (
      HealthCheckId::OidcSync,
      job(
        facts,
        facts.schedules.maybe_oidc_sync,
        facts.oidc_sync_last_success,
      ),
    ),
    (
      HealthCheckId::Retention,
      job(
        facts,
        facts.schedules.maybe_retention,
        facts.retention_last_success,
      ),
    ),
    (
      HealthCheckId::PartitionMaintenance,
      job(
        facts,
        facts.schedules.maybe_partition_maintenance,
        facts.partition_last_success,
      ),
    ),
    (HealthCheckId::Backup, backup(facts)),
    (HealthCheckId::Certificate, certificate(facts)),
    (HealthCheckId::DiskSpace, disk(facts)),
  ]
}

/// What a dependency nobody has called yet counts as.
#[derive(Clone, Copy)]
enum Absent {
  /// Not used yet, so nothing is wrong.
  Healthy,
  /// Not part of this instance.
  NotConfigured,
}

fn dependency(facts: &Facts, name: &DependencyName, absent: Absent) -> Observation {
  let verdict = match facts.dependencies.iter().find(|entry| entry.dependency == *name) {
    None => match absent {
      Absent::Healthy => Verdict::Healthy,
      Absent::NotConfigured => Verdict::NotConfigured,
    },
    Some(entry) => match entry.state {
      DependencyState::Ok => Verdict::Healthy,
      DependencyState::Down => Verdict::Unhealthy,
      // The next call decides; until then the earlier verdict stands.
      DependencyState::Recovering | DependencyState::Unknown(_) => Verdict::NoData,
    },
  };
  Observation::new(verdict)
}

fn push(providers: &[PushFacts]) -> Observation {
  let mut configured = providers
    .iter()
    .filter(|provider| provider.is_configured)
    .peekable();
  if configured.peek().is_none() {
    return Observation::new(Verdict::NotConfigured);
  }
  Observation::new(if configured.any(|provider| provider.is_failing) {
    Verdict::Unhealthy
  } else {
    Verdict::Healthy
  })
}

fn media(facts: &Facts) -> Observation {
  let maybe_last_contact = match facts.media {
    MediaServerFacts::NotMonitored => return Observation::new(Verdict::NotConfigured),
    MediaServerFacts::Reporting { maybe_last_contact } => maybe_last_contact,
  };
  let Some(last_contact) = maybe_last_contact else {
    // It reports as soon as it is up; still nothing after the grace time is silence.
    return Observation::new(if is_in_grace(facts) {
      Verdict::NoData
    } else {
      Verdict::Unhealthy
    });
  };
  let silent_for = facts.now - OffsetDateTime::UNIX_EPOCH - Duration::milliseconds(last_contact.0);
  Observation::with_value(
    if silent_for > SFU_SILENT_AFTER {
      Verdict::Unhealthy
    } else {
      Verdict::Healthy
    },
    silent_for.whole_seconds().max(0),
  )
}

/// A job that missed several runs. Before its first run since the start, the start is the point it
/// is measured from, and the delay to its first run and the grace time are added: a restart does
/// not make a job late.
fn job(
  facts: &Facts,
  maybe_schedule: Option<JobSchedule>,
  maybe_last: Option<OffsetDateTime>,
) -> Observation {
  let Some(schedule) = maybe_schedule else {
    return Observation::new(Verdict::NotConfigured);
  };
  let allowed = schedule.interval * JOB_MISSED_RUNS;
  let Some(last) = maybe_last else {
    let waited = facts.now - facts.started_at;
    return Observation::new(
      if waited > allowed + schedule.initial_delay + facts.startup_grace {
        Verdict::Unhealthy
      } else {
        Verdict::NoData
      },
    );
  };
  let age = facts.now - last;
  Observation::with_value(
    if age > allowed {
      Verdict::Unhealthy
    } else {
      Verdict::Healthy
    },
    age.whole_seconds().max(0),
  )
}

fn backup(facts: &Facts) -> Observation {
  match facts.heartbeats.backup {
    Reading::NotConfigured => Observation::new(Verdict::NotConfigured),
    Reading::Missing | Reading::Invalid => Observation::new(Verdict::NoData),
    Reading::Value(last_success) => {
      // A clock a little ahead of ours is no reason to doubt the backup.
      let age = (facts.now.unix_timestamp() - last_success).max(0);
      let is_old = age > i64::try_from(facts.settings.backup_max_age_secs).unwrap_or(i64::MAX);
      Observation::with_value(verdict(!is_old), age)
    },
  }
}

fn certificate(facts: &Facts) -> Observation {
  match facts.heartbeats.certificate {
    Reading::NotConfigured => Observation::new(Verdict::NotConfigured),
    Reading::Missing | Reading::Invalid => Observation::new(Verdict::NoData),
    Reading::Value(not_after) => {
      let days_left = (not_after - facts.now.unix_timestamp()).div_euclid(SECONDS_PER_DAY);
      Observation::with_value(
        verdict(days_left >= i64::from(facts.settings.certificate_min_days)),
        days_left,
      )
    },
  }
}

fn disk(facts: &Facts) -> Observation {
  match facts.heartbeats.disk_percent {
    Reading::NotConfigured => Observation::new(Verdict::NotConfigured),
    Reading::Missing | Reading::Invalid => Observation::new(Verdict::NoData),
    Reading::Value(percent) => Observation::with_value(
      verdict(percent <= i64::from(facts.settings.disk_max_percent)),
      percent,
    ),
  }
}

fn verdict(is_fine: bool) -> Verdict {
  if is_fine {
    Verdict::Healthy
  } else {
    Verdict::Unhealthy
  }
}

fn is_in_grace(facts: &Facts) -> bool {
  facts.now - facts.started_at <= facts.startup_grace
}

#[cfg(test)]
mod tests {
  use super::*;

  mod dependencies {
    use super::*;

    #[test]
    fn a_dependency_is_judged_by_its_breaker() {
      let down = [health(DependencyName::Database, DependencyState::Down)];
      assert_eq!(
        verdict_of(&facts_with(&down), HealthCheckId::Database),
        Verdict::Unhealthy
      );
      let ok = [health(DependencyName::Database, DependencyState::Ok)];
      assert_eq!(
        verdict_of(&facts_with(&ok), HealthCheckId::Database),
        Verdict::Healthy
      );
    }

    #[test]
    fn a_breaker_that_is_probing_again_changes_nothing() {
      let probing = [health(
        DependencyName::IdentityProvider,
        DependencyState::Recovering,
      )];
      assert_eq!(
        verdict_of(&facts_with(&probing), HealthCheckId::IdentityProvider),
        Verdict::NoData
      );
    }

    #[test]
    fn what_is_not_in_use_is_fine_or_not_set_up() {
      let none = facts_with(&[]);
      assert_eq!(verdict_of(&none, HealthCheckId::Database), Verdict::Healthy);
      assert_eq!(
        verdict_of(&none, HealthCheckId::PushGateway),
        Verdict::NotConfigured
      );
      assert_eq!(
        verdict_of(&none, HealthCheckId::ObjectStorage),
        Verdict::NotConfigured
      );
    }
  }

  mod push_and_media {
    use super::*;

    #[test]
    fn one_failing_push_service_makes_the_check_fail() {
      let mut facts = facts_with(&[]);
      let providers = [
        PushFacts {
          is_configured: true,
          is_failing: false,
        },
        PushFacts {
          is_configured: true,
          is_failing: true,
        },
        PushFacts {
          is_configured: false,
          is_failing: true,
        },
      ];
      facts.push = providers.to_vec();
      assert_eq!(verdict_of(&facts, HealthCheckId::PushService), Verdict::Unhealthy);
      let fine = [PushFacts {
        is_configured: true,
        is_failing: false,
      }];
      facts.push = fine.to_vec();
      assert_eq!(verdict_of(&facts, HealthCheckId::PushService), Verdict::Healthy);
      // A provider that is failing but not set up is not part of the instance.
      let unused = [PushFacts {
        is_configured: false,
        is_failing: true,
      }];
      facts.push = unused.to_vec();
      assert_eq!(
        verdict_of(&facts, HealthCheckId::PushService),
        Verdict::NotConfigured
      );
    }

    #[test]
    fn the_media_server_is_silent_after_two_minutes() {
      let mut facts = facts_with(&[]);
      let contact = |seconds_ago: i64| MediaServerFacts::Reporting {
        maybe_last_contact: Some(UnixMillis((facts.now.unix_timestamp() - seconds_ago) * 1000)),
      };
      facts.media = contact(10);
      assert_eq!(verdict_of(&facts, HealthCheckId::MediaServer), Verdict::Healthy);
      facts.media = contact(121);
      assert_eq!(verdict_of(&facts, HealthCheckId::MediaServer), Verdict::Unhealthy);
      facts.media = MediaServerFacts::NotMonitored;
      assert_eq!(
        verdict_of(&facts, HealthCheckId::MediaServer),
        Verdict::NotConfigured
      );
    }

    #[test]
    fn a_media_server_that_has_not_reported_since_the_start_gets_the_grace_time() {
      let mut facts = facts_with(&[]);
      facts.media = MediaServerFacts::Reporting {
        maybe_last_contact: None,
      };
      facts.started_at = facts.now - Duration::seconds(30);
      assert_eq!(verdict_of(&facts, HealthCheckId::MediaServer), Verdict::NoData);
      facts.started_at = facts.now - Duration::hours(1);
      assert_eq!(verdict_of(&facts, HealthCheckId::MediaServer), Verdict::Unhealthy);
    }
  }

  mod jobs {
    use super::*;

    #[test]
    fn a_job_is_late_after_three_missed_runs() {
      let mut facts = facts_with(&[]);
      facts.schedules.maybe_oidc_sync = Some(schedule(Duration::minutes(10), Duration::ZERO));
      facts.oidc_sync_last_success = Some(facts.now - Duration::minutes(29));
      assert_eq!(verdict_of(&facts, HealthCheckId::OidcSync), Verdict::Healthy);
      facts.oidc_sync_last_success = Some(facts.now - Duration::minutes(31));
      assert_eq!(verdict_of(&facts, HealthCheckId::OidcSync), Verdict::Unhealthy);
    }

    #[test]
    fn a_job_that_has_not_run_since_a_restart_is_not_late_until_its_time_and_the_grace_are_up() {
      let mut facts = facts_with(&[]);
      facts.schedules.maybe_retention = Some(schedule(Duration::hours(1), Duration::minutes(10)));
      facts.started_at = facts.now - Duration::minutes(10);
      assert_eq!(verdict_of(&facts, HealthCheckId::Retention), Verdict::NoData);
      // 3 h of runs, 10 min to the first one, 5 min of grace.
      facts.started_at = facts.now - Duration::minutes(3 * 60 + 14);
      assert_eq!(verdict_of(&facts, HealthCheckId::Retention), Verdict::NoData);
      facts.started_at = facts.now - Duration::minutes(3 * 60 + 16);
      assert_eq!(verdict_of(&facts, HealthCheckId::Retention), Verdict::Unhealthy);
    }

    #[test]
    fn a_job_that_is_not_scheduled_is_not_set_up() {
      assert_eq!(
        verdict_of(&facts_with(&[]), HealthCheckId::Retention),
        Verdict::NotConfigured
      );
    }
  }

  mod deployment_reports {
    use super::*;

    #[test]
    fn without_a_heartbeat_directory_none_of_them_is_set_up() {
      let facts = facts_with(&[]);
      for check in [
        HealthCheckId::Backup,
        HealthCheckId::Certificate,
        HealthCheckId::DiskSpace,
      ] {
        assert_eq!(verdict_of(&facts, check), Verdict::NotConfigured);
      }
    }

    #[test]
    fn a_report_that_has_not_come_yet_or_cannot_be_read_gives_no_verdict() {
      let mut facts = facts_with(&[]);
      let heartbeats = Heartbeats {
        backup: Reading::Missing,
        certificate: Reading::Invalid,
        disk_percent: Reading::Missing,
      };
      facts.heartbeats = heartbeats;
      for check in [
        HealthCheckId::Backup,
        HealthCheckId::Certificate,
        HealthCheckId::DiskSpace,
      ] {
        assert_eq!(verdict_of(&facts, check), Verdict::NoData);
      }
    }

    #[test]
    fn a_backup_older_than_the_limit_fails_and_a_clock_ahead_does_not() {
      let mut facts = facts_with(&[]);
      let now = facts.now.unix_timestamp();
      let hours = |hours: i64| now - hours * 3600;
      let old = Heartbeats {
        backup: Reading::Value(hours(37)),
        ..Heartbeats::default()
      };
      facts.heartbeats = old;
      let observation = observation_of(&facts, HealthCheckId::Backup);
      assert_eq!(observation.verdict, Verdict::Unhealthy);
      assert_eq!(observation.maybe_value, Some(37 * 3600));
      let recent = Heartbeats {
        backup: Reading::Value(hours(35)),
        ..Heartbeats::default()
      };
      facts.heartbeats = recent;
      assert_eq!(verdict_of(&facts, HealthCheckId::Backup), Verdict::Healthy);
      let ahead = Heartbeats {
        backup: Reading::Value(now + 90),
        ..Heartbeats::default()
      };
      facts.heartbeats = ahead;
      assert_eq!(observation_of(&facts, HealthCheckId::Backup).maybe_value, Some(0));
    }

    #[test]
    fn a_certificate_with_fewer_days_than_the_limit_fails() {
      let mut facts = facts_with(&[]);
      let now = facts.now.unix_timestamp();
      let days = |days: i64| now + days * SECONDS_PER_DAY + 60;
      let close = Heartbeats {
        certificate: Reading::Value(days(13)),
        ..Heartbeats::default()
      };
      facts.heartbeats = close;
      let observation = observation_of(&facts, HealthCheckId::Certificate);
      assert_eq!(
        (observation.verdict, observation.maybe_value),
        (Verdict::Unhealthy, Some(13))
      );
      let enough = Heartbeats {
        certificate: Reading::Value(days(14)),
        ..Heartbeats::default()
      };
      facts.heartbeats = enough;
      assert_eq!(verdict_of(&facts, HealthCheckId::Certificate), Verdict::Healthy);
      let expired = Heartbeats {
        certificate: Reading::Value(now - 5),
        ..Heartbeats::default()
      };
      facts.heartbeats = expired;
      let observation = observation_of(&facts, HealthCheckId::Certificate);
      assert_eq!(
        (observation.verdict, observation.maybe_value),
        (Verdict::Unhealthy, Some(-1))
      );
    }

    #[test]
    fn a_disk_fuller_than_the_limit_fails() {
      let mut facts = facts_with(&[]);
      let full = Heartbeats {
        disk_percent: Reading::Value(86),
        ..Heartbeats::default()
      };
      facts.heartbeats = full;
      assert_eq!(verdict_of(&facts, HealthCheckId::DiskSpace), Verdict::Unhealthy);
      let fine = Heartbeats {
        disk_percent: Reading::Value(85),
        ..Heartbeats::default()
      };
      facts.heartbeats = fine;
      assert_eq!(verdict_of(&facts, HealthCheckId::DiskSpace), Verdict::Healthy);
    }
  }

  #[test]
  fn every_check_has_a_verdict_in_the_order_of_the_list() {
    let facts = facts_with(&[]);
    let checks: Vec<HealthCheckId> = observe(&facts).into_iter().map(|(check, _)| check).collect();
    assert_eq!(checks, ALL_CHECKS);
  }

  // --- helpers -------------------------------------------------------------------------------

  fn facts_with(dependencies: &[DependencyHealth]) -> Facts {
    let now = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_800_000_000);
    Facts {
      now,
      started_at: now - Duration::days(30),
      startup_grace: Duration::minutes(5),
      settings: HealthAlertSettings::default(),
      dependencies: dependencies.to_vec(),
      has_object_storage: false,
      push: Vec::new(),
      media: MediaServerFacts::NotMonitored,
      schedules: JobSchedules::default(),
      oidc_sync_last_success: None,
      retention_last_success: None,
      partition_last_success: None,
      heartbeats: Heartbeats::default(),
    }
  }

  fn health(dependency: DependencyName, state: DependencyState) -> DependencyHealth {
    DependencyHealth { dependency, state }
  }

  fn schedule(interval: Duration, initial_delay: Duration) -> JobSchedule {
    JobSchedule {
      interval,
      initial_delay,
    }
  }

  fn observation_of(facts: &Facts, check: HealthCheckId) -> Observation {
    observe(facts)
      .into_iter()
      .find(|(id, _)| *id == check)
      .map(|(_, observation)| observation)
      .expect("every check is observed")
  }

  fn verdict_of(facts: &Facts, check: HealthCheckId) -> Verdict {
    observation_of(facts, check).verdict
  }
}
