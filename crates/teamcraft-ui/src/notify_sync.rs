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
  ListSpacesResponse, NotificationLevel, NotificationSettings, PushMode, SpaceNotificationSettings,
  UnixMillis, Weekday,
};

use crate::auth::{ApiMethod, Authenticator};
use crate::clock::{end_of_local_day, local_utc_offset_minutes, now_ms};
use crate::model::TimestampMs;
use crate::prefs::{ConversationNotify, DndSchedule, MuteFor, NotifyLevel, Preferences, PushWhen};
use crate::settings::Settings;

const MS_PER_HOUR: i64 = 3_600_000;
/// "Forever" is a mute time this far away; the server only knows times.
const FOREVER_MS: i64 = 100 * 365 * 24 * MS_PER_HOUR;
/// Mutes longer than this count as forever when read back.
const FOREVER_THRESHOLD_MS: i64 = 10 * 365 * 24 * MS_PER_HOUR;
const WORKDAYS: [Weekday; 5] = [
  Weekday::Monday,
  Weekday::Tuesday,
  Weekday::Wednesday,
  Weekday::Thursday,
  Weekday::Friday,
];
const EVERY_DAY: [Weekday; 7] = [
  Weekday::Monday,
  Weekday::Tuesday,
  Weekday::Wednesday,
  Weekday::Thursday,
  Weekday::Friday,
  Weekday::Saturday,
  Weekday::Sunday,
];

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
    settings.prefs.update(|prefs| {
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
        .map(|previous| global_from_prefs(prefs, previous, now_ms()));
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
      ..notes.dnd.clone()
    },
    dnd_from_server,
  );
  prefs.privacy.share_read_receipts = server.send_read_receipts;
}

/// Local preferences as server settings; fields the UI has no control for come from `previous`.
pub fn global_from_prefs(
  prefs: &Preferences,
  previous: &NotificationSettings,
  now: TimestampMs,
) -> NotificationSettings {
  let notes = &prefs.notifications;
  NotificationSettings {
    auto_follow_mentioned_threads: notes.auto_follow_on_channel_mention,
    push_mode: match notes.push_when {
      PushWhen::Always => PushMode::Always,
      PushWhen::AwayOrOffline => PushMode::AwayOnly,
    },
    send_read_receipts: prefs.privacy.share_read_receipts,
    maybe_dnd_schedule: dnd_to_server(&notes.dnd, previous.maybe_dnd_schedule.as_ref(), now),
  }
}

pub fn dnd_to_server(dnd: &DndSchedule, previous: Option<&DndWire>, now: TimestampMs) -> Option<DndWire> {
  if !dnd.is_enabled {
    return None;
  }
  let (start_minute, end_minute) = (parse_minutes(&dnd.from)?, parse_minutes(&dnd.to)?);
  let weekdays = if dnd.weekdays_only {
    WORKDAYS.to_vec()
  } else {
    EVERY_DAY.to_vec()
  };
  Some(DndWire {
    utc_offset_minutes: local_utc_offset_minutes(now),
    windows: vec![DndWindow {
      weekdays,
      start_minute,
      end_minute,
    }],
    allow_user_ids: previous
      .map(|previous| previous.allow_user_ids.clone())
      .unwrap_or_default(),
    // Kept from what another device stored until this UI edits them itself.
    maybe_time_zone: previous.and_then(|previous| previous.maybe_time_zone.clone()),
    allows_urgent: previous.is_some_and(|previous| previous.allows_urgent),
  })
}

pub fn dnd_from_server(server: &DndWire) -> DndSchedule {
  let defaults = DndSchedule::default();
  let Some(window) = server.windows.first() else {
    return DndSchedule {
      is_enabled: false,
      ..defaults
    };
  };
  DndSchedule {
    is_enabled: true,
    from: format_minutes(window.start_minute),
    to: format_minutes(window.end_minute),
    weekdays_only: window.weekdays.len() == WORKDAYS.len()
      && !window.weekdays.contains(&Weekday::Saturday)
      && !window.weekdays.contains(&Weekday::Sunday),
  }
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

fn parse_minutes(text: &str) -> Option<u16> {
  let (hours, minutes) = text.split_once(':')?;
  let (hours, minutes) = (
    hours.trim().parse::<u16>().ok()?,
    minutes.trim().parse::<u16>().ok()?,
  );
  (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

fn format_minutes(minutes: u16) -> String {
  format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_792_540_800_000;

  #[test]
  fn global_settings_round_trip_through_the_preferences() {
    let mut prefs = Preferences::default();
    let server = NotificationSettings {
      auto_follow_mentioned_threads: false,
      push_mode: PushMode::Always,
      send_read_receipts: false,
      maybe_dnd_schedule: Some(DndWire {
        utc_offset_minutes: 0,
        windows: vec![DndWindow {
          weekdays: WORKDAYS.to_vec(),
          start_minute: 22 * 60,
          end_minute: 6 * 60 + 30,
        }],
        allow_user_ids: Vec::new(),
        ..DndWire::default()
      }),
    };
    apply_global(&mut prefs, &server);
    assert_eq!(prefs.notifications.push_when, PushWhen::Always);
    assert!(!prefs.privacy.share_read_receipts);
    assert_eq!(prefs.notifications.dnd.from, "22:00");
    assert_eq!(prefs.notifications.dnd.to, "06:30");
    assert!(prefs.notifications.dnd.weekdays_only);
    assert_eq!(global_from_prefs(&prefs, &server, NOW), server);
  }

  #[test]
  fn a_disabled_schedule_is_no_schedule() {
    let dnd = DndSchedule::default();
    assert!(!dnd.is_enabled);
    assert_eq!(dnd_to_server(&dnd, None, NOW), None);
  }

  #[test]
  fn the_allow_list_survives_edits_the_ui_cannot_make() {
    let sender = crate::model::id_from_raw(5);
    let previous = DndWire {
      utc_offset_minutes: 60,
      windows: Vec::new(),
      allow_user_ids: vec![sender],
      ..DndWire::default()
    };
    let dnd = DndSchedule {
      is_enabled: true,
      ..DndSchedule::default()
    };
    let wire = dnd_to_server(&dnd, Some(&previous), NOW).expect("schedule");
    assert_eq!(wire.allow_user_ids, vec![sender]);
  }

  #[test]
  fn malformed_times_do_not_produce_a_schedule() {
    for bad in ["", "7", "25:00", "08:75", "ab:cd"] {
      let dnd = DndSchedule {
        is_enabled: true,
        from: bad.to_owned(),
        ..DndSchedule::default()
      };
      assert_eq!(dnd_to_server(&dnd, None, NOW), None, "{bad}");
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
