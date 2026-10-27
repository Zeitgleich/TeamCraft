//! System notifications of the signed-in app (notifications.md, "Desktop notifications"): new
//! messages, ringing and missed calls, meeting reminders, and what the user does with them.
//!
//! The policies are pure and tested elsewhere ([`crate::notifier`], [`crate::call_alerts`],
//! [`crate::meeting_reminders`]); this module feeds them from the backend and hands the results to
//! the desktop shell or the browser ([`tauri::show_notification`]). Clicks and buttons come back as
//! [`NotificationActionEvent`]s, from the shell's event or the service worker's message, and go
//! through the normal paths: replies are sent like composer messages (end-to-end encrypted, through
//! the outbox), Accept is the in-app card's Accept.
//!
//! In a browser only the tab that owns the account's store notifies (e2ee.md §9.4); every tab
//! records which conversation it shows in front, so the owner stays quiet about a conversation the
//! user reads in another tab.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use teamcraft_client_api::actions::{MessagePriority, OutgoingMessage};
use teamcraft_client_api::activity::{JoinRequest, JoinRequestStanding};
use teamcraft_client_api::call_window::{CallWindow, CallWindowTheme};
use teamcraft_client_api::os_notify::{
  NOTIFICATION_ACTION_EVENT, NotificationAction, NotificationActionEvent, NotificationButton,
  NotificationPicture, NotificationSound, NotificationTag, OsNotification, OsNotificationKind, ReplyField,
  TagTarget,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::tauri;
use crate::call_alerts::{
  CallAlert, CallAlerts, RingInput, decide_missed, decide_ring, may_name_names, ring_end,
};
use crate::calls::{CallKind, CallPhase, CallRequest, IncomingCall};
use crate::clock::{local_utc_offset_minutes, now_ms};
use crate::i18n::I18n;
use crate::join_request_alerts::{AlertChanges, JoinRequestAlerts, tag as join_request_tag};
use crate::meeting_reminders::{
  DueReminder, FiredReminders, ReminderKind, ReminderSchedule, ReminderSettings,
};
use crate::meetings::MeetingsHub;
use crate::message_sounds::{MessageSounds, notification_sound};
use crate::model::{ConversationId, TimestampMs, UserId};
use crate::notification_pictures::picture_of;
use crate::notifier::{
  Decision, NotificationTitle, Notifier, PictureSubject, ReadWatcher, all_conversations,
  needs_channel_confirmation, notifications_for,
};
use crate::reactive::OwnerLiveness;
use crate::settings::{Theme, load_string, store_string};
use crate::state::{AppState, MainView};

/// How long another tab's "I show this conversation" counts without being renewed.
const TAB_FOCUS_FRESH_MS: i64 = 15_000;
/// How often a tab renews its record while it is in front.
const TAB_FOCUS_RENEW_MS: u64 = 5_000;
/// Delay before the meetings are loaded at start, so the first paint doesn't wait for them.
const FIRST_MEETINGS_LOAD: Duration = Duration::from_secs(3);
/// How early a meeting reminder may show: the clock is re-measured right before.
const DUE_SLACK_MS: i64 = 2_000;
/// Storage key prefix of the reminders shown on this device; the user ID follows.
const FIRED_REMINDERS_KEY: &str = "teamcraft.meeting_reminders.";
/// Storage key prefix of the conversation a tab shows in front; the user ID follows.
const TAB_FOCUS_KEY: &str = "teamcraft.focused_conversation.";
/// Storage key prefix of the join requests this device notified about; the user ID follows.
const JOIN_REQUESTS_KEY: &str = "teamcraft.join_request_alerts.";

/// Ringing calls of this device, shared by the watcher and the call handler (which tells it when
/// the user accepted or declined here).
#[derive(Clone, Copy)]
pub struct CallAlertsCell(StoredValue<CallAlerts, LocalStorage>);

/// Starts everything once after sign-in.
pub fn start(state: AppState, i18n: I18n) {
  let calls = CallAlertsCell(StoredValue::new_local(CallAlerts::default()));
  provide_context(calls);
  watch_messages(state, i18n, MessageSounds::provide());
  watch_join_requests(state, i18n);
  if tauri::is_available() {
    watch_calls(state, i18n, calls);
  } else {
    share_tab_focus(state);
  }
  if let Some(hub) = use_context::<MeetingsHub>() {
    watch_meetings(state, i18n, hub);
  }
  listen_for_actions(state, i18n);
}

impl CallAlertsCell {
  /// The user accepted or declined `conversation`'s call on this device.
  pub fn handled_here(&self, conversation: ConversationId) {
    self.0.update_value(|alerts| alerts.handled_here(conversation));
  }
}

// ---- Messages -------------------------------------------------------------------------------------

/// New messages, mentions and followed thread replies; withdrawn once read anywhere. Those that
/// notify play the message sound, once for a burst (#800).
fn watch_messages(state: AppState, i18n: I18n, sounds: MessageSounds) {
  let notifier = Notifier::default();
  let read_watcher = ReadWatcher::default();
  let settings = state.settings;
  Effect::new(move |_| {
    // Reading through the state subscribes to every backend change.
    let found = state.read(|backend| notifier.collect(backend));
    let read = state.read(|backend| {
      let unread: Vec<(ConversationId, u32)> = all_conversations(backend)
        .iter()
        .map(|conversation| (conversation.id, conversation.unread_count))
        .collect();
      read_watcher.newly_read(&unread)
    });
    if !read.is_empty() {
      leptos::task::spawn_local(async move { tauri::withdraw_notifications(&read).await });
    }
    if found.is_empty() || !is_notifying_tab() {
      return;
    }
    let prefs = settings.prefs.get_untracked().notifications;
    let current = state.current_conversation_id();
    let is_in_front = is_window_in_front();
    let me = state.me().id;
    let notifications = state.read(|backend| {
      notifications_for(
        found,
        &prefs,
        backend,
        |conversation| {
          (is_in_front && current == Some(conversation)) || is_shown_in_other_tab(me, conversation)
        },
        prefs.maybe_dnd_until,
      )
    });
    if !notifications.is_empty() {
      let is_in_call = state.call.phase.get_untracked() == CallPhase::Active;
      sounds.on_notifications(&prefs, is_in_call);
    }
    let sound = notification_sound(prefs.message_sound);
    for notification in notifications {
      let is_generic = notification.title == NotificationTitle::Generic;
      let title = match &notification.title {
        NotificationTitle::Generic => i18n.t("notify-new-message"),
        NotificationTitle::Sender(name) => name.clone(),
        NotificationTitle::InChannel { sender, channel } => {
          i18n.t_with("notify-in-channel", &[("name", sender), ("channel", channel)])
        },
        NotificationTitle::ThreadReply { sender } => i18n.t_with("notify-thread-reply", &[("name", sender)]),
      };
      // A generic notification is only its title; otherwise the fallback body says what happened.
      let maybe_body = notification
        .body
        .or_else(|| (!is_generic).then(|| i18n.t("notify-new-message")));
      // Browsers have no text field in notifications.
      let maybe_reply = (notification.can_reply && tauri::is_available()).then(|| ReplyField {
        placeholder: i18n.t("notify-reply-placeholder"),
        send_label: i18n.t("notify-action-send"),
      });
      let maybe_picture = notification
        .maybe_picture
        .and_then(|subject| state.read(|backend| picture_of(backend, subject)));
      let shown = OsNotification {
        tag: NotificationTag::conversation(notification.conversation),
        kind: OsNotificationKind::Message,
        title,
        maybe_body,
        actions: vec![button(
          NotificationAction::MarkRead,
          i18n.t("notify-action-mark-read"),
        )],
        maybe_reply,
        maybe_picture,
        sound,
      };
      leptos::task::spawn_local(async move { tauri::show_notification(&shown).await });
    }
  });
  // Opening a conversation withdraws its notifications, also those of thread replies and a missed
  // call from there.
  Effect::new(move |_| {
    if let Some(conversation) = state.current_conversation_id() {
      let tags = [
        NotificationTag::conversation(conversation),
        NotificationTag::missed_call(conversation),
      ];
      leptos::task::spawn_local(async move { tauri::withdraw_tags(&tags).await });
    }
  });
}

// ---- Join requests --------------------------------------------------------------------------------

/// Admins asking to join a channel the user owns (admin.md §2.4): one notification per request,
/// with Approve where the verified roster lists the admin and Decline always; withdrawn once the
/// request is gone. Each device notifies once per request, also across restarts.
fn watch_join_requests(state: AppState, i18n: I18n) {
  let alerts = StoredValue::new_local(None::<JoinRequestAlerts>);
  Effect::new(move |_| {
    let requests = state.read(|backend| backend.join_requests());
    // Only the tab that notifies keeps the record (tabs share it): another tab marking a request
    // as notified would keep the notifying one from ever showing it.
    if !is_notifying_tab() {
      return;
    }
    let key = format!("{JOIN_REQUESTS_KEY}{}", state.me().id);
    let Some(changes) = alerts.try_update_value(|maybe_alerts| {
      let alerts = maybe_alerts.get_or_insert_with(|| {
        load_string(&key)
          .map(|text| JoinRequestAlerts::from_json(&text))
          .unwrap_or_default()
      });
      let changes = alerts.update(&requests);
      store_string(&key, &alerts.to_json());
      changes
    }) else {
      return;
    };
    if changes == AlertChanges::default() {
      return;
    }
    let shown: Vec<OsNotification> = changes
      .new
      .iter()
      .map(|request| join_request_notification(state, i18n, request))
      .collect();
    let gone = changes.gone;
    leptos::task::spawn_local(async move {
      if !gone.is_empty() {
        tauri::withdraw_tags(&gone).await;
      }
      for notification in &shown {
        tauri::show_notification(notification).await;
      }
    });
  });
}

fn join_request_notification(state: AppState, i18n: I18n, request: &JoinRequest) -> OsNotification {
  let channel = state
    .read(|backend| backend.conversation(request.conversation))
    .map(|conversation| format!("#{}", conversation.name))
    .unwrap_or_default();
  let mut actions = Vec::new();
  if request.standing == JoinRequestStanding::Approvable {
    actions.push(button(
      NotificationAction::Approve,
      i18n.t("join-request-approve"),
    ));
  }
  actions.push(button(
    NotificationAction::Decline,
    i18n.t("join-request-decline"),
  ));
  OsNotification {
    tag: join_request_tag(request),
    kind: OsNotificationKind::JoinRequest,
    title: i18n.t_with(
      "notify-join-request",
      &[("name", &named(request)), ("channel", &channel)],
    ),
    maybe_body: Some(i18n.t("notify-join-request-body")),
    actions,
    maybe_reply: None,
    maybe_picture: person_picture_if_named(state, request.user),
    sound: NotificationSound::System,
  }
}

/// The admin's name with the username, so two people with one name differ.
fn named(request: &JoinRequest) -> String {
  match &request.maybe_username {
    Some(username) => format!("{} (@{username})", request.user_name),
    None => request.user_name.clone(),
  }
}

fn on_join_request_action(state: AppState, conversation: &str, user: &str, action: NotificationAction) {
  let (Ok(conversation), Ok(user)) = (conversation.parse(), user.parse()) else {
    return;
  };
  // Only while the request still waits (and, for Approve, may be approved here): a notification
  // left over from before is no reason to add anybody.
  let maybe_request = state.read(|backend| {
    backend
      .join_requests()
      .into_iter()
      .find(|request| request.conversation == conversation && request.user == user)
  });
  match (action, maybe_request) {
    (NotificationAction::Approve, Some(request)) if request.standing == JoinRequestStanding::Approvable => {
      // One click shares no more than a self-join would: history from the join on.
      state.write(|backend| {
        backend.approve_admin_join(conversation, user, teamcraft_proto::HistoryPolicy::FromJoin)
      });
    },
    (NotificationAction::Decline, Some(_)) => {
      state.write(|backend| backend.decline_admin_join(conversation, user));
    },
    _ => state.open_activity(),
  }
}

// ---- Calls ----------------------------------------------------------------------------------------

/// Desktop: a ringing call gets the ringtone and, while the window is not in front, the
/// incoming-call window (a setting, on by default) or else a notification with Accept and Decline,
/// and an attention request (#650). Leaving the window while it rings shows them then; coming back
/// takes them away again (the in-app card is on screen). All of it stops the moment the call stops
/// ringing here, and an unanswered call leaves a missed-call notification.
fn watch_calls(state: AppState, i18n: I18n, cell: CallAlertsCell) {
  let here = Rc::new(RefCell::new(RingingHere::default()));
  {
    let here = here.clone();
    Effect::new(move |_| {
      let incoming = state.read(|backend| backend.incoming_calls());
      let me = state.me().id;
      let alerts = cell.0.try_update_value(|alerts| {
        alerts.update(&incoming, |call| {
          let participants: Vec<_> = state
            .read(|backend| backend.call_participants(call.conversation))
            .into_iter()
            .map(|participant| participant.user)
            .collect();
          let maybe_participants = (!participants.is_empty()).then_some(participants.as_slice());
          ring_end(call, me, maybe_participants)
        })
      });
      for alert in alerts.unwrap_or_default() {
        apply_call_alert(state, i18n, &here, alert);
      }
    });
  }
  for event in ["blur", "visibilitychange"] {
    let here = here.clone();
    on_window_event(event, move || {
      if !is_window_in_front() {
        surface_ringing(state, i18n, &here);
      }
    });
  }
  on_window_event("focus", move || {
    let surfaced = std::mem::take(&mut here.borrow_mut().surfaced);
    if !surfaced.is_empty() {
      for conversation in surfaced {
        hide_surfaced(conversation);
      }
      leptos::task::spawn_local(tauri::request_attention(false));
    }
  });
}

/// What the call watcher keeps about the calls ringing on this device.
#[derive(Default)]
struct RingingHere {
  /// Calls that play the ringtone.
  ringtone_for: Vec<ConversationId>,
  /// Every call ringing here, so leaving the window can still show them.
  calls: Vec<IncomingCall>,
  /// Calls shown outside the window right now (call window or notification).
  surfaced: Vec<ConversationId>,
}

fn apply_call_alert(state: AppState, i18n: I18n, here: &Rc<RefCell<RingingHere>>, alert: CallAlert) {
  match alert {
    CallAlert::Ring(call) => {
      let prefs = state.settings.prefs.get_untracked().notifications;
      let Some(decision) =
        with_ring_input(state, &call, |input| decide_ring(input, prefs.plays_ringtone)).flatten()
      else {
        return;
      };
      here.borrow_mut().calls.push(call);
      if decision.shows_notification {
        surface(state, i18n, here, &call);
      }
      // A second call while the user is in one rings only as a notification ("call waiting").
      let is_in_call = state.call.phase.get_untracked() == CallPhase::Active;
      if decision.plays_ringtone && !is_in_call {
        let was_silent = here.borrow().ringtone_for.is_empty();
        here.borrow_mut().ringtone_for.push(call.conversation);
        if was_silent {
          leptos::task::spawn_local(tauri::set_ringtone(true));
        }
      }
    },
    CallAlert::StopRinging(conversation) => {
      let (had_ringtone, has_ringtone) = {
        let mut here = here.borrow_mut();
        let had_ringtone = !here.ringtone_for.is_empty();
        here.ringtone_for.retain(|other| *other != conversation);
        here.calls.retain(|call| call.conversation != conversation);
        here.surfaced.retain(|other| *other != conversation);
        (had_ringtone, !here.ringtone_for.is_empty())
      };
      // Withdrawn also when not surfaced by this page: one may be left from before a reload.
      hide_surfaced(conversation);
      if had_ringtone && !has_ringtone {
        leptos::task::spawn_local(tauri::set_ringtone(false));
      }
      if state.read(|backend| backend.incoming_calls().is_empty()) {
        leptos::task::spawn_local(tauri::request_attention(false));
      }
    },
    CallAlert::Missed(call) => {
      if is_window_in_front() {
        // The card was on screen; the call history lists it.
        return;
      }
      let decision = with_ring_input(state, &call, decide_missed);
      if decision != Some(Decision::Notify) {
        return;
      }
      let shown = missed_call_notification(state, i18n, &call);
      leptos::task::spawn_local(async move { tauri::show_notification(&shown).await });
    },
  }
}

/// The window went to the background while calls ring: shows those the rules let through.
fn surface_ringing(state: AppState, i18n: I18n, here: &Rc<RefCell<RingingHere>>) {
  let waiting: Vec<IncomingCall> = {
    let here = here.borrow();
    here
      .calls
      .iter()
      .filter(|call| !here.surfaced.contains(&call.conversation))
      .copied()
      .collect()
  };
  let prefs = state.settings.prefs.get_untracked().notifications;
  for call in waiting {
    let is_shown = with_ring_input(state, &call, |input| decide_ring(input, prefs.plays_ringtone))
      .flatten()
      .is_some_and(|decision| decision.shows_notification);
    if is_shown {
      surface(state, i18n, here, &call);
    }
  }
}

/// Shows a ringing call outside the window: the incoming-call window where the setting is on and
/// the shell can open it, the notification otherwise; and asks for attention.
fn surface(state: AppState, i18n: I18n, here: &Rc<RefCell<RingingHere>>, call: &IncomingCall) {
  if here.borrow().surfaced.contains(&call.conversation) {
    return;
  }
  here.borrow_mut().surfaced.push(call.conversation);
  let prefs = state.settings.prefs.get_untracked().notifications;
  let maybe_window = prefs
    .shows_call_window
    .then(|| call_window(state, i18n, call))
    .flatten();
  let notification = incoming_call_notification(state, i18n, call);
  let conversation = call.conversation;
  let here = here.clone();
  leptos::task::spawn_local(async move {
    let is_window_shown = match maybe_window {
      Some(window) => tauri::show_call_window(&window).await,
      None => false,
    };
    if !is_window_shown {
      tauri::show_notification(&notification).await;
    }
    // It stopped ringing (or the user came back) while the shell showed it.
    if !here.borrow().surfaced.contains(&conversation) {
      hide_surfaced(conversation);
    }
  });
  leptos::task::spawn_local(tauri::request_attention(true));
}

/// Takes a ringing call's window and notification away.
fn hide_surfaced(conversation: ConversationId) {
  let tag = NotificationTag::call(conversation);
  leptos::task::spawn_local(async move {
    tauri::close_call_window(&tag).await;
    tauri::withdraw_tags(&[tag]).await;
  });
}

/// Runs `decide` on the policy input of a call; `None` when the conversation is unknown.
fn with_ring_input<T>(
  state: AppState,
  call: &IncomingCall,
  decide: impl FnOnce(&RingInput) -> T,
) -> Option<T> {
  let conversation = state.read(|backend| backend.conversation(call.conversation))?;
  let me = state.me();
  let prefs = state.settings.prefs.get_untracked().notifications;
  let now = now_ms();
  let input = RingInput {
    prefs: &prefs,
    me: &me,
    conversation: &conversation,
    caller: call.caller,
    now,
    is_window_in_front: is_window_in_front(),
    utc_offset_minutes: local_utc_offset_minutes(now),
    maybe_dnd_until: prefs.maybe_dnd_until,
  };
  Some(decide(&input))
}

/// The caller's name where the workspace policy allows names on the lock screen.
fn caller_name(state: AppState, call: &IncomingCall) -> Option<String> {
  let mode = state.read(|backend| backend.push_content_mode());
  if !may_name_names(&mode) {
    return None;
  }
  state.read(|backend| backend.user(call.caller).map(|user| user.display_name))
}

/// The person's avatar (#543) where the workspace policy allows names on the lock screen: a face
/// names the person as the name does.
fn person_picture_if_named(state: AppState, user: UserId) -> Option<NotificationPicture> {
  let mode = state.read(|backend| backend.push_content_mode());
  if !may_name_names(&mode) {
    return None;
  }
  state.read(|backend| picture_of(backend, PictureSubject::Person(user)))
}

fn incoming_call_notification(state: AppState, i18n: I18n, call: &IncomingCall) -> OsNotification {
  let maybe_body = caller_name(state, call).map(|name| {
    let key = if call.with_video {
      "incoming-video"
    } else {
      "incoming-audio"
    };
    i18n.t_with(key, &[("name", &name)])
  });
  let mut actions = vec![button(
    NotificationAction::AcceptAudio,
    i18n.t("incoming-accept-audio"),
  )];
  // As on the in-app card: the desktop answers video calls with audio first.
  if !tauri::is_available() {
    actions.push(button(
      NotificationAction::AcceptVideo,
      i18n.t("incoming-accept-video"),
    ));
  }
  actions.push(button(NotificationAction::Decline, i18n.t("incoming-decline")));
  OsNotification {
    tag: NotificationTag::call(call.conversation),
    kind: OsNotificationKind::IncomingCall,
    title: i18n.t("incoming-title"),
    maybe_body,
    actions,
    maybe_reply: None,
    maybe_picture: person_picture_if_named(state, call.caller),
    sound: NotificationSound::System,
  }
}

/// What the incoming-call window shows: the in-app card's content, in the app's theme. `None` when
/// the caller is unknown (the notification shows instead).
fn call_window(state: AppState, i18n: I18n, call: &IncomingCall) -> Option<CallWindow> {
  let caller = state.read(|backend| backend.user(call.caller))?;
  let key = if call.with_video {
    "incoming-video"
  } else {
    "incoming-audio"
  };
  let theme = match state.settings.theme.get_untracked() {
    Theme::System => CallWindowTheme::System,
    Theme::Light => CallWindowTheme::Light,
    Theme::Dark => CallWindowTheme::Dark,
  };
  Some(CallWindow {
    tag: NotificationTag::call(call.conversation),
    title: i18n.t("incoming-title"),
    text: i18n.t_with(key, &[("name", &caller.display_name)]),
    initials: caller.initials(),
    avatar_hue: caller.avatar_hue,
    decline_label: i18n.t("incoming-decline"),
    accept_label: i18n.t("incoming-accept-audio"),
    theme,
  })
}

fn missed_call_notification(state: AppState, i18n: I18n, call: &IncomingCall) -> OsNotification {
  let maybe_body =
    caller_name(state, call).map(|name| i18n.t_with("notify-missed-call-from", &[("name", &name)]));
  OsNotification {
    tag: NotificationTag::missed_call(call.conversation),
    kind: OsNotificationKind::MissedCall,
    title: i18n.t("notify-missed-call"),
    maybe_body,
    actions: vec![button(
      NotificationAction::CallBack,
      i18n.t("notify-action-call-back"),
    )],
    maybe_reply: None,
    maybe_picture: person_picture_if_named(state, call.caller),
    sound: NotificationSound::System,
  }
}

// ---- Meetings -------------------------------------------------------------------------------------

/// Reminders before and at the start of the user's meetings. One timer sleeps until the next
/// reminder; the list is read again when the gateway reports a meeting change, after a reconnect,
/// on wake, and right before showing a reminder, so a cancelled or moved meeting stays quiet.
fn watch_meetings(state: AppState, i18n: I18n, hub: MeetingsHub) {
  let watch = Rc::new(MeetingWatch {
    state,
    i18n,
    hub,
    timer: RefCell::new(None),
  });
  // The list may change elsewhere (the meetings view, an RSVP) and so may the settings: plan again
  // without fetching.
  {
    let watch = watch.clone();
    Effect::new(move |_| {
      hub.meetings.track();
      state.settings.prefs.track();
      watch.plan_next();
    });
  }
  // A meeting created, moved or cancelled elsewhere (gateway `meeting_update`), and a reconnect
  // (updates in between may be lost): read the list again now. This replaces a periodic refresh.
  {
    let watch = watch.clone();
    Effect::new(move |maybe_last: Option<u64>| {
      let revision = state.read(|backend| backend.meetings_revision());
      if maybe_last.is_some_and(|last| last != revision) {
        watch.check();
      }
      revision
    });
  }
  // Laptops sleep: on wake or reconnect the timer may be late or stale, so check now.
  for event in ["online", "pageshow"] {
    let watch = watch.clone();
    on_window_event(event, move || watch.check());
  }
  leptos::prelude::set_timeout(move || watch.check(), FIRST_MEETINGS_LOAD);
}

/// What the meeting reminders keep between checks.
struct MeetingWatch {
  state: AppState,
  i18n: I18n,
  hub: MeetingsHub,
  timer: RefCell<Option<TimeoutHandle>>,
}

impl MeetingWatch {
  /// Fetches the list, shows what is due on it and plans the next check.
  fn check(self: &Rc<Self>) {
    let watch = self.clone();
    leptos::task::spawn_local(async move {
      // A timer of a shell that is gone (signed out): nothing to remind of any more.
      if !watch.is_alive() {
        return;
      }
      let me = watch.state.me().id;
      watch.hub.refresh(me, now_ms()).await;
      if !watch.is_alive() {
        return;
      }
      crate::meetings::finish_pending_edits(watch.state, watch.hub);
      remind_due(watch.state, watch.i18n, watch.hub);
      watch.plan_next();
    });
  }

  fn is_alive(&self) -> bool {
    self.state.is_alive() && !self.hub.clock.is_disposed()
  }

  fn plan_next(self: &Rc<Self>) {
    if let Some(handle) = self.timer.borrow_mut().take() {
      handle.clear();
    }
    let clock = self.hub.clock.get_untracked();
    let fired = load_fired(self.state);
    let server_now = clock.now(now_ms());
    let next = self.hub.meetings.with_untracked(|meetings| {
      ReminderSchedule {
        meetings,
        me: self.state.me().id,
        settings: reminder_settings(self.state),
      }
      .next_due_at(server_now, &fired)
    });
    let wait_ms = u64::try_from(clock.local_time_of(next) - now_ms()).unwrap_or(0);
    let watch = self.clone();
    match set_timeout_with_handle(move || watch.check(), Duration::from_millis(wait_ms)) {
      Ok(handle) => *self.timer.borrow_mut() = Some(handle),
      Err(_) => tracing::debug!("could not plan the next meeting reminder"),
    }
  }
}

/// Shows the reminders due now on the freshly loaded list and remembers them.
fn remind_due(state: AppState, i18n: I18n, hub: MeetingsHub) {
  if !is_notifying_tab() {
    return;
  }
  // A timer that fires a moment early (a re-measured clock) still counts.
  let server_now = hub.clock.get_untracked().now(now_ms()) + DUE_SLACK_MS;
  let mut fired = load_fired(state);
  let due = hub.meetings.with_untracked(|meetings| {
    ReminderSchedule {
      meetings,
      me: state.me().id,
      settings: reminder_settings(state),
    }
    .due(server_now, &fired)
  });
  if due.is_empty() {
    return;
  }
  let mode = state.read(|backend| backend.push_content_mode());
  for reminder in &due {
    fired.insert(reminder);
    let shown = meeting_notification(i18n, reminder, server_now, may_name_names(&mode));
    leptos::task::spawn_local(async move { tauri::show_notification(&shown).await });
  }
  fired.forget_old(server_now);
  store_string(&fired_key(state), &fired.to_json());
}

fn meeting_notification(
  i18n: I18n,
  reminder: &DueReminder,
  server_now: TimestampMs,
  may_name: bool,
) -> OsNotification {
  let minutes = ((reminder.starts_at - server_now).max(0) + 59_999) / 60_000;
  let title = match reminder.kind {
    ReminderKind::Before => i18n.t_with("notify-meeting-soon", &[("minutes", &minutes.to_string())]),
    ReminderKind::AtStart => i18n.t("notify-meeting-now"),
  };
  OsNotification {
    tag: NotificationTag::meeting(reminder.meeting),
    kind: OsNotificationKind::MeetingReminder,
    title,
    maybe_body: may_name.then(|| reminder.title.clone()),
    actions: vec![button(NotificationAction::JoinMeeting, i18n.t("meetings-join"))],
    maybe_reply: None,
    maybe_picture: None,
    sound: NotificationSound::System,
  }
}

fn reminder_settings(state: AppState) -> ReminderSettings {
  let prefs = state.settings.prefs.get_untracked().notifications;
  ReminderSettings {
    maybe_minutes_before: prefs.maybe_meeting_reminder_minutes,
    notifies_at_start: prefs.notifies_meeting_start,
  }
}

fn fired_key(state: AppState) -> String {
  format!("{FIRED_REMINDERS_KEY}{}", state.me().id)
}

fn load_fired(state: AppState) -> FiredReminders {
  load_string(&fired_key(state))
    .map(|text| FiredReminders::from_json(&text))
    .unwrap_or_default()
}

// ---- Actions --------------------------------------------------------------------------------------

/// Clicks and buttons: the desktop shell's event, or the service worker's message in a browser.
fn listen_for_actions(state: AppState, i18n: I18n) {
  if tauri::is_available() {
    tauri::listen_text(
      NOTIFICATION_ACTION_EVENT,
      move |payload| match serde_json::from_str::<NotificationActionEvent>(&payload) {
        Ok(event) => handle_action(state, i18n, event),
        Err(error) => tracing::debug!(%error, "unreadable notification action"),
      },
    );
    return;
  }
  let Some(window) = web_sys::window() else { return };
  let liveness = OwnerLiveness::of_current_owner();
  let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |message: web_sys::MessageEvent| {
    if !liveness.is_alive() {
      return;
    }
    let data = message.data();
    let text = |key: &str| {
      js_sys::Reflect::get(&data, &key.into())
        .ok()
        .and_then(|value| value.as_string())
    };
    if text("type").as_deref() != Some("notification_action") {
      return;
    }
    let (Some(tag), Some(action)) = (text("tag"), text("action")) else {
      return;
    };
    handle_action(
      state,
      i18n,
      NotificationActionEvent {
        tag: NotificationTag::from_raw(tag),
        action: NotificationAction::from_key(&action),
        maybe_text: text("text"),
      },
    );
  });
  if window
    .navigator()
    .service_worker()
    .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

/// Carries out what the user chose in a notification (also usable by the tray menu).
pub fn handle_action(state: AppState, i18n: I18n, event: NotificationActionEvent) {
  match event.tag.target() {
    TagTarget::Conversation(id) => {
      if let Ok(conversation) = id.parse() {
        on_message_action(state, conversation, event.action, event.maybe_text);
      }
    },
    TagTarget::Call(id) => {
      if let Ok(conversation) = id.parse() {
        on_call_action(state, conversation, event.action);
      }
    },
    TagTarget::MissedCall(id) => {
      if let Ok(conversation) = id.parse::<ConversationId>() {
        state.open_conversation(conversation);
        if event.action == NotificationAction::CallBack {
          call_back(state, conversation);
        }
      }
    },
    TagTarget::Meeting(id) => on_meeting_action(state, i18n, &id, event.action),
    TagTarget::JoinRequest { conversation, user } => {
      on_join_request_action(state, &conversation, &user, event.action);
    },
    TagTarget::Reminder => {},
  }
}

fn on_message_action(
  state: AppState,
  conversation: ConversationId,
  action: NotificationAction,
  maybe_text: Option<String>,
) {
  match action {
    NotificationAction::MarkRead => mark_read(state, conversation),
    NotificationAction::Reply => {
      let text = maybe_text.unwrap_or_default();
      if text.trim().is_empty() {
        return;
      }
      if needs_confirmation(state, conversation, &text) {
        // `@channel` in a big conversation needs the composer's confirmation: open it with the text.
        state.write(|backend| backend.set_draft(conversation, None, &text));
        state.open_conversation(conversation);
        return;
      }
      state.write(|backend| {
        backend.send_message(OutgoingMessage {
          conversation,
          maybe_thread: None,
          body: text,
          maybe_reply_to: None,
          priority: MessagePriority::Normal,
          wants_acknowledgement: false,
          is_silent: false,
          also_send_to_channel: false,
          files: Vec::new(),
        });
      });
      mark_read(state, conversation);
    },
    _ => state.open_conversation(conversation),
  }
}

fn needs_confirmation(state: AppState, conversation: ConversationId, text: &str) -> bool {
  let members = state
    .read(|backend| backend.conversation(conversation))
    .map_or(0, |conversation| conversation.member_ids.len());
  let threshold = state
    .settings
    .prefs
    .get_untracked()
    .notifications
    .confirm_channel_over;
  needs_channel_confirmation(text, members, threshold)
}

fn mark_read(state: AppState, conversation: ConversationId) {
  state.write(|backend| backend.mark_read(conversation));
  leptos::task::spawn_local(async move { tauri::withdraw_notifications(&[conversation]).await });
}

fn on_call_action(state: AppState, conversation: ConversationId, action: NotificationAction) {
  let maybe_call = state.read(|backend| {
    backend
      .incoming_calls()
      .into_iter()
      .find(|call| call.conversation == conversation)
  });
  let Some(call) = maybe_call else {
    // It stopped ringing meanwhile: show where it was.
    state.open_conversation(conversation);
    return;
  };
  match action {
    NotificationAction::AcceptAudio => state.accept_call(call, false),
    NotificationAction::AcceptVideo => state.accept_call(call, true),
    NotificationAction::Decline => {
      if state
        .call
        .incoming
        .get_untracked()
        .map(|incoming| incoming.conversation)
        == Some(conversation)
      {
        state.call.decline_incoming();
      } else {
        state.call.emit(crate::calls::CallEvent::Decline(conversation));
      }
    },
    // A click on the notification: the in-app card is on screen now.
    _ => state.open_conversation(conversation),
  }
}

fn call_back(state: AppState, conversation: ConversationId) {
  if !state.read(|backend| backend.has_calls()) || state.call.phase.get_untracked() != CallPhase::Idle {
    return;
  }
  let kind = match state
    .read(|backend| backend.conversation(conversation))
    .map(|c| c.kind)
  {
    Some(crate::model::ConversationKind::Direct) | None => CallKind::Direct,
    Some(_) => CallKind::Group,
  };
  state.call.prepare(CallRequest { conversation, kind }, false);
}

fn on_meeting_action(state: AppState, i18n: I18n, id: &str, action: NotificationAction) {
  let maybe_conversation = use_context::<MeetingsHub>().and_then(|hub| {
    hub.meetings.with_untracked(|meetings| {
      meetings
        .iter()
        .find(|meeting| meeting.id.to_string() == id)
        .map(|meeting| meeting.conversation_id)
    })
  });
  match (action, maybe_conversation) {
    (NotificationAction::JoinMeeting, Some(conversation)) => {
      crate::components::meetings_view::join_meeting(state, i18n, conversation);
    },
    _ => state.view.set(MainView::Meetings),
  }
}

// ---- Tabs and windows -----------------------------------------------------------------------------

fn button(action: NotificationAction, label: String) -> NotificationButton {
  NotificationButton { action, label }
}

/// Whether this page shows notifications: always in the desktop app; in a browser only the tab
/// that owns the account's store, so several tabs notify once.
fn is_notifying_tab() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    tauri::is_available() || crate::web_core::is_notifying_tab()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    true
  }
}

/// The window is focused and visible.
fn is_window_in_front() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| {
      document.has_focus().unwrap_or(false)
        && document.visibility_state() == web_sys::VisibilityState::Visible
    })
}

/// Browser: records which conversation this tab shows while it is in front, for the tab that
/// notifies (another tab of the same browser).
fn share_tab_focus(state: AppState) {
  let me = state.me().id;
  let record = move || {
    let maybe_conversation = state.current_conversation_id().filter(|_| is_window_in_front());
    let key = format!("{TAB_FOCUS_KEY}{me}");
    match maybe_conversation {
      Some(conversation) => store_string(&key, &format!("{conversation}|{}", now_ms())),
      None => {
        // Only clear what this tab wrote: another tab may be in front now.
        if !is_window_in_front()
          && load_string(&key).is_some_and(|value| {
            state
              .current_conversation_id()
              .is_some_and(|current| value.starts_with(&format!("{current}|")))
          })
        {
          store_string(&key, "");
        }
      },
    }
  };
  Effect::new(move |_| {
    state.view.track();
    record();
  });
  for event in ["focus", "blur", "pagehide"] {
    on_window_event(event, record);
  }
  // Only a tab in front has a record to renew; blur and pagehide end it when it leaves.
  crate::reactive::use_interval_while_visible(TAB_FOCUS_RENEW_MS, record);
}

/// Whether another tab of this browser shows `conversation` in front right now.
fn is_shown_in_other_tab(me: crate::model::UserId, conversation: ConversationId) -> bool {
  if tauri::is_available() {
    return false;
  }
  let Some(value) = load_string(&format!("{TAB_FOCUS_KEY}{me}")) else {
    return false;
  };
  let Some((shown, at)) = value.split_once('|') else {
    return false;
  };
  let is_fresh = at
    .parse::<i64>()
    .is_ok_and(|at| now_ms().saturating_sub(at) < TAB_FOCUS_FRESH_MS);
  is_fresh && shown == conversation.to_string()
}

fn on_window_event(event: &str, callback: impl Fn() + 'static) {
  let Some(window) = web_sys::window() else { return };
  let liveness = OwnerLiveness::of_current_owner();
  let closure = Closure::<dyn FnMut()>::new(move || {
    if liveness.is_alive() {
      callback();
    }
  });
  if window
    .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    closure.forget();
  }
}
