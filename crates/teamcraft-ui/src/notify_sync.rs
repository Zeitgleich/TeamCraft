//! Keeps the notification preferences that the server enforces in step with the server.
//!
//! Push routing runs on the server, so push mode, the do-not-disturb schedule, read receipts,
//! thread auto-follow and the per-conversation level and mute time are stored there. The rest of the
//! notification preferences (desktop level, keywords, highlights) stay on the device. On start the
//! server's values are applied to the local preferences; afterwards every local change of a server
//! field is sent. Both directions are pure mappings, tested below.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{
  ConversationNotificationSettings, DndSchedule as DndWire, DndWindow, ListConversationsResponse,
  ListSpacesResponse, MINUTES_PER_DAY, NotificationLevel, NotificationSettings, PushMode,
  SpaceNotificationSettings, UnixMillis,
};

use crate::auth::{ApiMethod, Authenticator};
use crate::clock::{end_of_local_day, local_time_zone, local_utc_offset_minutes, now_ms};
use crate::model::TimestampMs;
use crate::prefs::{ConversationNotify, DndSchedule, EVERY_DAY, MuteFor, NotifyLevel, Preferences, PushWhen};
use crate::reactive::UpdateIfAlive;
use crate::settings::Settings;

const MS_PER_HOUR: i64 = 3_600_000;
/// "Forever" is a mute time this far away; the server only knows times.
const FOREVER_MS: i64 = 100 * 365 * 24 * MS_PER_HOUR;
/// Mutes longer than this count as forever when read back.
const FOREVER_THRESHOLD_MS: i64 = 10 * 365 * 24 * MS_PER_HOUR;
/// Talks to the server on behalf of the settings screens. Provided as context by the data gate.
#[derive(Clone)]
pub struct NotificationSync {
  auth: Rc<dyn Authenticator>,
}

/// What the server last confirmed, to send only real changes.
#[derive(Default)]
struct Confirmed {
  is_loaded: bool,
  maybe_global: Option<NotificationSettings>,
  conversations: BTreeMap<String, ConversationNotify>,
  spaces: BTreeMap<String, ConversationNotify>,
}

impl NotificationSync {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self { auth }
  }

  /// Applies the server's values to the preferences, then sends later changes. Call once.
  pub fn start(&self, settings: Settings) {
    let confirmed = Rc::new(RefCell::new(Confirmed::default()));
    let sync = self.clone();
    let loading = confirmed.clone();
    spawn_local(async move {
      sync.load(settings, &loading).await;
    });
    let sync = self.clone();
    Effect::new(move |_| {
      let prefs = settings.prefs.get();
      sync.send_changes(&prefs, &confirmed);
    });
  }

  async fn load(&self, settings: Settings, confirmed: &Rc<RefCell<Confirmed>>) {
    let global = self
      .get::<NotificationSettings>("/api/v1/me/notification-settings")
      .await;
    let conversations = self
      .get::<ListConversationsResponse>("/api/v1/conversations")
      .await;
    let spaces = self.get::<ListSpacesResponse>("/api/v1/spaces").await;
    let now = now_ms();
    let mut state = confirmed.borrow_mut();
    settings.prefs.update_if_alive(|prefs| {
      if let Some(list) = &spaces {
        for space in &list.spaces {
          if let Some(mine) = &space.maybe_my_notifications {
            let notify = space_from_server(mine, now);
            let id = space.id.to_string();
            prefs.notifications.set_space(&id, notify);
            state.spaces.insert(id, notify);
          }
        }
      }
      if let Some(global) = &global {
        apply_global(prefs, global);
      }
      if let Some(list) = &conversations {
        for conversation in &list.conversations {
          if let Some(membership) = &conversation.maybe_my_membership {
            let notify = notify_from_server(
              membership.notification_level.clone(),
              membership.maybe_muted_until,
              membership.is_notification_inherited,
              now,
            );
            let id = conversation.id.to_string();
            prefs.notifications.set_conversation(&id, notify);
            state.conversations.insert(id, notify);
          }
        }
      }
    });
    state.maybe_global = global;
    // A failed read must not let defaults overwrite the server's values.
    state.is_loaded = state.maybe_global.is_some();
  }

  fn send_changes(&self, prefs: &Preferences, confirmed: &Rc<RefCell<Confirmed>>) {
    let (maybe_global, changed_conversations, changed_spaces) = {
      let state = confirmed.borrow();
      if !state.is_loaded {
        return;
      }
      let previous = state.maybe_global.clone();
      let wanted = previous
        .as_ref()
        .map(|previous| global_from_prefs(prefs, previous, &LocalZone::current(now_ms())));
      let global = wanted.filter(|wanted| Some(wanted) != previous.as_ref());
      let ids: std::collections::BTreeSet<&String> = prefs
        .notifications
        .conversations
        .keys()
        .chain(state.conversations.keys())
        .collect();
      let changed: Vec<(String, ConversationNotify)> = ids
        .into_iter()
        .filter_map(|id| {
          let wanted = prefs.notifications.conversation(id);
          let known = state.conversations.get(id).copied().unwrap_or_default();
          (wanted != known).then(|| (id.clone(), wanted))
        })
        .collect();
      let space_ids: std::collections::BTreeSet<&String> = prefs
        .notifications
        .spaces
        .keys()
        .chain(state.spaces.keys())
        .collect();
      let changed_spaces: Vec<(String, ConversationNotify)> = space_ids
        .into_iter()
        .filter_map(|id| {
          let wanted = prefs.notifications.space(id);
          let known = state.spaces.get(id).copied().unwrap_or_default();
          (wanted != known).then(|| (id.clone(), wanted))
        })
        .collect();
      (global, changed, changed_spaces)
    };
    if let Some(global) = maybe_global {
      confirmed.borrow_mut().maybe_global = Some(global.clone());
      self.put(
        "/api/v1/me/notification-settings".to_owned(),
        serde_json::to_string(&global).ok(),
      );
    }
    for (id, notify) in changed_spaces {
      confirmed.borrow_mut().spaces.insert(id.clone(), notify);
      let wire = space_to_server(&notify, now_ms());
      self.put(
        format!("/api/v1/spaces/{id}/notification-settings"),
        serde_json::to_string(&wire).ok(),
      );
    }
    for (id, notify) in changed_conversations {
      confirmed.borrow_mut().conversations.insert(id.clone(), notify);
      let wire = notify_to_server(&notify, now_ms());
      self.put(
        format!("/api/v1/conversations/{id}/notification-settings"),
        serde_json::to_string(&wire).ok(),
      );
    }
  }

  async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
    match self.auth.api_get(path).await {
      Ok(response) if response.status == 200 => serde_json::from_str(&response.body)
        .inspect_err(|error| tracing::warn!(path, %error, "unreadable notification settings"))
        .ok(),
      Ok(response) => {
        tracing::warn!(
          path,
          status = response.status,
          "notification settings were refused"
        );
        None
      },
      Err(error) => {
        tracing::warn!(path, %error, "could not read notification settings");
        None
      },
    }
  }

  fn put(&self, path: String, maybe_body: Option<String>) {
    let auth = self.auth.clone();
    spawn_local(async move {
      match auth.api_request(ApiMethod::Put, &path, maybe_body).await {
        Ok(response) if response.status < 300 => {},
        Ok(response) => tracing::warn!(
          path,
          status = response.status,
          "the server refused a notification setting"
        ),
        Err(error) => tracing::warn!(path, %error, "could not save a notification setting"),
      }
    });
  }
}

/// Server values into the local preferences (device-only fields stay).
pub fn apply_global(prefs: &mut Preferences, server: &NotificationSettings) {
  let notes = &mut prefs.notifications;
  notes.push_when = match server.push_mode {
    PushMode::Always => PushWhen::Always,
    PushMode::AwayOnly | PushMode::Unknown(_) => PushWhen::AwayOrOffline,
  };
  notes.auto_follow_on_channel_mention = server.auto_follow_mentioned_threads;
  notes.dnd = server.maybe_dnd_schedule.as_ref().map_or_else(
    || DndSchedule {
      is_enabled: false,
      allow_user_ids: Vec::new(),
      allows_urgent: false,
      ..notes.dnd.clone()
    },
    |schedule| dnd_from_server(schedule, &notes.dnd),
  );
  prefs.privacy.share_read_receipts = server.send_read_receipts;
  // Servers from before the setting say nothing: attention is used there.
  prefs.notifications.uses_desktop_attention = server.maybe_uses_desktop_attention.unwrap_or(true);
}

/// Local preferences as server settings; fields the UI has no control for come from `previous`.
pub fn global_from_prefs(
  prefs: &Preferences,
  previous: &NotificationSettings,
  zone: &LocalZone,
) -> NotificationSettings {
  let notes = &prefs.notifications;
  NotificationSettings {
    auto_follow_mentioned_threads: notes.auto_follow_on_channel_mention,
    push_mode: match notes.push_when {
      PushWhen::Always => PushMode::Always,
      PushWhen::AwayOrOffline => PushMode::AwayOnly,
    },
    send_read_receipts: prefs.privacy.share_read_receipts,
    maybe_dnd_schedule: dnd_to_server(&notes.dnd, zone, previous.maybe_dnd_schedule.as_ref()),
    maybe_uses_desktop_attention: Some(notes.uses_desktop_attention),
  }
}

/// Most ranges and allowed people the server stores (it refuses more).
pub const MAX_DND_WINDOWS: usize = 14;
pub const MAX_DND_ALLOWED_USERS: usize = 50;

/// Where the device is: the schedule is evaluated in this time zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalZone {
  pub utc_offset_minutes: i16,
  /// IANA name from the browser, when it tells.
  pub maybe_time_zone: Option<String>,
}

impl LocalZone {
  pub fn current(now: TimestampMs) -> Self {
    Self {
      utc_offset_minutes: local_utc_offset_minutes(now),
      maybe_time_zone: local_time_zone(),
    }
  }
}

/// The editor's schedule for the server. Without a schedule but with overrides (allowed people,
/// urgent messages, which also apply to manual DND) it is a schedule without windows. Ranges the
/// server would refuse (no weekday, empty) are left out. A time zone another device stored stays when
/// this one cannot tell its own.
pub fn dnd_to_server(dnd: &DndSchedule, zone: &LocalZone, previous: Option<&DndWire>) -> Option<DndWire> {
  let has_overrides = !dnd.allow_user_ids.is_empty() || dnd.allows_urgent;
  if !dnd.is_enabled && !has_overrides {
    return None;
  }
  let windows = if dnd.is_enabled {
    dnd
      .windows
      .iter()
      .filter(|window| is_valid_window(window))
      .take(MAX_DND_WINDOWS)
      .map(|window| DndWindow {
        weekdays: EVERY_DAY
          .iter()
          .filter(|day| window.weekdays.contains(day))
          .cloned()
          .collect(),
        ..window.clone()
      })
      .collect()
  } else {
    Vec::new()
  };
  Some(DndWire {
    utc_offset_minutes: zone.utc_offset_minutes,
    maybe_time_zone: zone
      .maybe_time_zone
      .clone()
      .or_else(|| previous.and_then(|previous| previous.maybe_time_zone.clone())),
    windows,
    allow_user_ids: dnd
      .allow_user_ids
      .iter()
      .copied()
      .take(MAX_DND_ALLOWED_USERS)
      .collect(),
    allows_urgent: dnd.allows_urgent,
    // Not edited here yet: keep what another device chose.
    urgent_scope: previous
      .map(|previous| previous.urgent_scope.clone())
      .unwrap_or_default(),
  })
}

/// The server's schedule for the editor. A schedule without windows is off; the editor then keeps
/// its own ranges to start from.
pub fn dnd_from_server(server: &DndWire, local: &DndSchedule) -> DndSchedule {
  let is_enabled = !server.windows.is_empty();
  DndSchedule {
    is_enabled,
    windows: if is_enabled {
      server.windows.clone()
    } else {
      local.windows.clone()
    },
    allow_user_ids: server.allow_user_ids.clone(),
    allows_urgent: server.allows_urgent,
  }
}

/// A range the server accepts: at least one weekday, start and end inside the day and different.
pub fn is_valid_window(window: &DndWindow) -> bool {
  window.weekdays.iter().any(|day| EVERY_DAY.contains(day))
    && window.start_minute < MINUTES_PER_DAY
    && window.end_minute < MINUTES_PER_DAY
    && window.start_minute != window.end_minute
}

/// `HH:MM` of an `<input type="time">` as minutes since midnight.
pub fn parse_minutes(text: &str) -> Option<u16> {
  let (hours, minutes) = text.split_once(':')?;
  let (hours, minutes) = (
    hours.trim().parse::<u16>().ok()?,
    minutes.trim().parse::<u16>().ok()?,
  );
  (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

pub fn format_minutes(minutes: u16) -> String {
  format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A conversation's setting for the server. The default setting means "follow the space", which the
/// server stores as no setting of its own.
pub fn notify_to_server(notify: &ConversationNotify, now: TimestampMs) -> ConversationNotificationSettings {
  if *notify == ConversationNotify::default() {
    return ConversationNotificationSettings {
      level: NotificationLevel::Mentions,
      maybe_muted_until: None,
      is_inherited: true,
    };
  }
  let level = match notify.maybe_level {
    // Only a mute time was set: the server still stores a level next to it.
    None | Some(NotifyLevel::Mentions) => NotificationLevel::Mentions,
    Some(NotifyLevel::All) => NotificationLevel::All,
    Some(NotifyLevel::Nothing) => NotificationLevel::None,
  };
  ConversationNotificationSettings {
    level,
    maybe_muted_until: mute_until(notify.mute, now).map(UnixMillis),
    is_inherited: false,
  }
}

pub fn notify_from_server(
  level: NotificationLevel,
  maybe_muted_until: Option<UnixMillis>,
  is_inherited: bool,
  now: TimestampMs,
) -> ConversationNotify {
  if is_inherited {
    return ConversationNotify::default();
  }
  let mute = mute_from(maybe_muted_until, now);
  let maybe_level = match level {
    NotificationLevel::All => Some(NotifyLevel::All),
    NotificationLevel::None => Some(NotifyLevel::Nothing),
    // With a mute time the level is only a placeholder; without one the member chose "mentions".
    NotificationLevel::Mentions if mute == MuteFor::Off => Some(NotifyLevel::Mentions),
    NotificationLevel::Mentions | NotificationLevel::Unknown(_) => None,
  };
  ConversationNotify { maybe_level, mute }
}

/// A space's setting for the server: the level is optional, so "no choice" stays distinct from
/// "mentions".
pub fn space_to_server(notify: &ConversationNotify, now: TimestampMs) -> SpaceNotificationSettings {
  SpaceNotificationSettings {
    maybe_level: notify.maybe_level.map(|level| match level {
      NotifyLevel::All => NotificationLevel::All,
      NotifyLevel::Mentions => NotificationLevel::Mentions,
      NotifyLevel::Nothing => NotificationLevel::None,
    }),
    maybe_muted_until: mute_until(notify.mute, now).map(UnixMillis),
  }
}

pub fn space_from_server(server: &SpaceNotificationSettings, now: TimestampMs) -> ConversationNotify {
  ConversationNotify {
    maybe_level: server.maybe_level.as_ref().and_then(|level| match level {
      NotificationLevel::All => Some(NotifyLevel::All),
      NotificationLevel::Mentions => Some(NotifyLevel::Mentions),
      NotificationLevel::None => Some(NotifyLevel::Nothing),
      NotificationLevel::Unknown(_) => None,
    }),
    mute: mute_from(server.maybe_muted_until, now),
  }
}

fn mute_until(mute: MuteFor, now: TimestampMs) -> Option<TimestampMs> {
  match mute {
    MuteFor::Off => None,
    MuteFor::OneHour => Some(now + MS_PER_HOUR),
    MuteFor::UntilTomorrow => Some(end_of_local_day(now)),
    MuteFor::Forever => Some(now + FOREVER_MS),
  }
}

fn mute_from(maybe_muted_until: Option<UnixMillis>, now: TimestampMs) -> MuteFor {
  match maybe_muted_until.map(|until| until.0 - now) {
    Some(remaining) if remaining > FOREVER_THRESHOLD_MS => MuteFor::Forever,
    Some(remaining) if remaining > MS_PER_HOUR => MuteFor::UntilTomorrow,
    Some(remaining) if remaining > 0 => MuteFor::OneHour,
    _ => MuteFor::Off,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  use teamcraft_proto::Weekday;

  const NOW: TimestampMs = 1_792_540_800_000;
  const WORKDAYS: [Weekday; 5] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
  ];

  mod dnd {
    use super::*;

    #[test]
    fn global_settings_round_trip_through_the_preferences() {
      let mut prefs = Preferences::default();
      let server = NotificationSettings {
        auto_follow_mentioned_threads: false,
        push_mode: PushMode::Always,
        send_read_receipts: false,
        maybe_dnd_schedule: Some(DndWire {
          utc_offset_minutes: 60,
          maybe_time_zone: Some("Europe/Berlin".into()),
          windows: vec![night(WORKDAYS.to_vec()), weekend_afternoon()],
          allow_user_ids: vec![crate::model::id_from_raw(5)],
          allows_urgent: true,
          urgent_scope: teamcraft_proto::UrgentScope::Direct,
        }),
        maybe_uses_desktop_attention: Some(true),
      };
      apply_global(&mut prefs, &server);
      assert_eq!(prefs.notifications.push_when, PushWhen::Always);
      assert!(!prefs.privacy.share_read_receipts);
      let dnd = &prefs.notifications.dnd;
      assert!(dnd.is_enabled);
      assert_eq!(dnd.windows.len(), 2);
      assert!(dnd.allows_urgent);
      assert_eq!(global_from_prefs(&prefs, &server, &berlin()), server);
    }

    #[test]
    fn several_ranges_with_their_own_weekdays_go_to_the_server() {
      let dnd = DndSchedule {
        is_enabled: true,
        windows: vec![night(WORKDAYS.to_vec()), weekend_afternoon()],
        ..DndSchedule::default()
      };
      let wire = dnd_to_server(&dnd, &berlin(), None).expect("schedule");
      assert_eq!(wire.windows, dnd.windows);
      assert_eq!(wire.utc_offset_minutes, 60);
      assert_eq!(wire.maybe_time_zone.as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn weekdays_are_sent_in_calendar_order() {
      let dnd = DndSchedule {
        is_enabled: true,
        windows: vec![night(vec![Weekday::Sunday, Weekday::Monday])],
        ..DndSchedule::default()
      };
      let wire = dnd_to_server(&dnd, &berlin(), None).expect("schedule");
      assert_eq!(wire.windows[0].weekdays, vec![Weekday::Monday, Weekday::Sunday]);
    }

    #[test]
    fn a_disabled_schedule_without_overrides_is_no_schedule() {
      let dnd = DndSchedule::default();
      assert!(!dnd.is_enabled);
      assert_eq!(dnd_to_server(&dnd, &berlin(), None), None);
    }

    #[test]
    fn overrides_without_a_schedule_are_a_schedule_without_windows() {
      let dnd = DndSchedule {
        allows_urgent: true,
        allow_user_ids: vec![crate::model::id_from_raw(5)],
        ..DndSchedule::default()
      };
      let wire = dnd_to_server(&dnd, &berlin(), None).expect("overrides");
      assert!(wire.windows.is_empty());
      assert!(wire.allows_urgent);
      let back = dnd_from_server(&wire, &DndSchedule::default());
      assert!(!back.is_enabled);
      assert_eq!(back.windows, DndSchedule::default().windows);
      assert_eq!(back.allow_user_ids, dnd.allow_user_ids);
    }

    #[test]
    fn ranges_the_server_would_refuse_are_left_out() {
      let dnd = DndSchedule {
        is_enabled: true,
        windows: vec![
          night(Vec::new()),
          DndWindow {
            weekdays: WORKDAYS.to_vec(),
            start_minute: 600,
            end_minute: 600,
          },
          DndWindow {
            weekdays: WORKDAYS.to_vec(),
            start_minute: 1440,
            end_minute: 60,
          },
          weekend_afternoon(),
        ],
        ..DndSchedule::default()
      };
      let wire = dnd_to_server(&dnd, &berlin(), None).expect("schedule");
      assert_eq!(wire.windows, vec![weekend_afternoon()]);
    }

    #[test]
    fn the_server_limits_are_kept() {
      let dnd = DndSchedule {
        is_enabled: true,
        windows: vec![weekend_afternoon(); MAX_DND_WINDOWS + 3],
        allow_user_ids: (1..=60).map(crate::model::id_from_raw).collect(),
        ..DndSchedule::default()
      };
      let wire = dnd_to_server(&dnd, &berlin(), None).expect("schedule");
      assert_eq!(wire.windows.len(), MAX_DND_WINDOWS);
      assert_eq!(wire.allow_user_ids.len(), MAX_DND_ALLOWED_USERS);
    }

    #[test]
    fn a_time_zone_from_another_device_stays_when_this_one_has_none() {
      let previous = DndWire {
        maybe_time_zone: Some("America/New_York".into()),
        ..DndWire::default()
      };
      let dnd = DndSchedule {
        is_enabled: true,
        ..DndSchedule::default()
      };
      let zone = LocalZone {
        utc_offset_minutes: -240,
        maybe_time_zone: None,
      };
      let wire = dnd_to_server(&dnd, &zone, Some(&previous)).expect("schedule");
      assert_eq!(wire.maybe_time_zone.as_deref(), Some("America/New_York"));
      assert_eq!(wire.utc_offset_minutes, -240);
      let own = dnd_to_server(&dnd, &berlin(), Some(&previous)).expect("schedule");
      assert_eq!(own.maybe_time_zone.as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn no_server_schedule_turns_the_overrides_off_but_keeps_the_ranges() {
      let mut prefs = Preferences::default();
      prefs.notifications.dnd = DndSchedule {
        is_enabled: true,
        windows: vec![weekend_afternoon()],
        allow_user_ids: vec![crate::model::id_from_raw(5)],
        allows_urgent: true,
      };
      let server = NotificationSettings {
        auto_follow_mentioned_threads: true,
        push_mode: PushMode::AwayOnly,
        send_read_receipts: true,
        maybe_dnd_schedule: None,
        maybe_uses_desktop_attention: None,
      };
      apply_global(&mut prefs, &server);
      let dnd = &prefs.notifications.dnd;
      assert!(!dnd.is_enabled && !dnd.allows_urgent && dnd.allow_user_ids.is_empty());
      assert_eq!(dnd.windows, vec![weekend_afternoon()]);
    }

    #[test]
    fn editor_times_parse_and_format() {
      assert_eq!(parse_minutes("06:30"), Some(390));
      assert_eq!(format_minutes(390), "06:30");
      for bad in ["", "7", "25:00", "08:75", "ab:cd"] {
        assert_eq!(parse_minutes(bad), None, "{bad}");
      }
    }

    fn night(weekdays: Vec<Weekday>) -> DndWindow {
      DndWindow {
        weekdays,
        start_minute: 22 * 60,
        end_minute: 6 * 60 + 30,
      }
    }

    fn weekend_afternoon() -> DndWindow {
      DndWindow {
        weekdays: vec![Weekday::Saturday, Weekday::Sunday],
        start_minute: 12 * 60,
        end_minute: 18 * 60,
      }
    }

    fn berlin() -> LocalZone {
      LocalZone {
        utc_offset_minutes: 60,
        maybe_time_zone: Some("Europe/Berlin".into()),
      }
    }
  }

  #[test]
  fn conversation_levels_map_both_ways() {
    for notify in [
      ConversationNotify::default(),
      ConversationNotify {
        maybe_level: Some(NotifyLevel::All),
        mute: MuteFor::Off,
      },
      ConversationNotify {
        maybe_level: Some(NotifyLevel::Nothing),
        mute: MuteFor::Forever,
      },
      ConversationNotify {
        maybe_level: None,
        mute: MuteFor::OneHour,
      },
    ] {
      let wire = notify_to_server(&notify, NOW);
      assert_eq!(
        notify_from_server(wire.level, wire.maybe_muted_until, wire.is_inherited, NOW),
        notify
      );
    }
  }

  #[test]
  fn an_expired_mute_reads_back_as_off() {
    let past = Some(UnixMillis(NOW - 1));
    assert_eq!(
      notify_from_server(NotificationLevel::Mentions, past, false, NOW).mute,
      MuteFor::Off
    );
  }

  #[test]
  fn the_default_follows_the_space_and_an_explicit_mentions_level_does_not() {
    let inherited = notify_to_server(&ConversationNotify::default(), NOW);
    assert!(inherited.is_inherited);
    let explicit = ConversationNotify {
      maybe_level: Some(NotifyLevel::Mentions),
      mute: MuteFor::Off,
    };
    let wire = notify_to_server(&explicit, NOW);
    assert!(!wire.is_inherited);
    assert_eq!(
      notify_from_server(wire.level, wire.maybe_muted_until, false, NOW),
      explicit
    );
  }

  #[test]
  fn space_settings_map_both_ways() {
    for notify in [
      ConversationNotify::default(),
      ConversationNotify {
        maybe_level: Some(NotifyLevel::Nothing),
        mute: MuteFor::Off,
      },
      ConversationNotify {
        maybe_level: Some(NotifyLevel::Mentions),
        mute: MuteFor::Forever,
      },
      ConversationNotify {
        maybe_level: None,
        mute: MuteFor::OneHour,
      },
    ] {
      assert_eq!(space_from_server(&space_to_server(&notify, NOW), NOW), notify);
    }
  }
}
