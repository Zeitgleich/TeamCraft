//! Settings > Notifications: one master switch, this device's notification status with a test
//! notification, then what notifies the user, do not disturb and per-conversation levels. The
//! values live in `NotificationPrefs` (see `prefs.rs`); `notify_sync` sends them to the server.

use leptos::prelude::*;
use teamcraft_client_api::os_notify::{NotificationTag, OsNotification, OsNotificationKind};
use teamcraft_proto::DndWindow;

use crate::auth::tauri;
use crate::clock::local_time_zone;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::settings_account::WeekdayChips;
use crate::components::settings_rows::{
  ListFilter, MAX_LIST_ENTRIES, SelectRow, SettingRow, SettingsCard, SwitchRow, matches_filter, row_id,
  slug_options,
};
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::model::{Conversation, User, UserId};
use crate::notify_sync::{
  MAX_DND_ALLOWED_USERS, MAX_DND_WINDOWS, format_minutes, is_valid_window, parse_minutes,
};
use crate::prefs::{
  DndSchedule, MuteFor, NotificationPrefs, NotifyLevel, PushWhen, ThreadReplies, default_dnd_window,
};
use crate::reactive::SetIfAlive;
use crate::settings::{load_string, store_string};
use crate::state::use_app_state;

/// The tag of the test notification; a newer test replaces the last one.
const TEST_TAG: &str = "settings-test";

#[component]
pub fn NotificationsSection() -> impl IntoView {
  let state = use_app_state();
  let prefs = state.settings.prefs;
  let notes = Memo::new(move |_| prefs.get().notifications);
  let edit =
    move |change: Box<dyn FnOnce(&mut NotificationPrefs)>| prefs.update(|all| change(&mut all.notifications));
  let is_on = Signal::derive(move || is_enabled(&notes.get()));
  let is_off = Signal::derive(move || !is_on.get());

  view! {
    <SettingsCard heading="notif-master-title">
      <SwitchRow
        label="notif-master"
        hint="notif-master-hint"
        value=is_on
        on_change=Callback::new(move |on: bool| edit(Box::new(move |n| set_enabled(n, on))))
      />
    </SettingsCard>
    <ThisDeviceCard />
    <SettingsCard heading="notif-what">
      <SelectRow
        label="notif-desktop"
        hint="notif-level-hint"
        options=level_options()
        is_disabled=is_off
        value=Signal::derive(move || notes.get().desktop.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(level) = NotifyLevel::from_slug(&slug) {
            edit(Box::new(move |n| n.desktop = level));
          }
        })
      />
      <SwitchRow
        label="notif-preview"
        hint="notif-preview-hint"
        is_disabled=is_off
        value=Signal::derive(move || notes.get().show_preview)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.show_preview = value)))
      />
    </SettingsCard>
    <SettingsCard heading="notif-mentions">
      <SwitchRow
        label="notif-first-name"
        hint="notif-first-name-hint"
        is_disabled=is_off
        value=Signal::derive(move || notes.get().first_name_is_mention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.first_name_is_mention = value)))
      />
      <SwitchRow
        label="notif-channel-mentions"
        hint="notif-channel-mentions-hint"
        is_disabled=is_off
        value=Signal::derive(move || notes.get().channel_mentions_notify)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.channel_mentions_notify = value)))
      />
      <SelectRow
        label="notif-confirm-channel"
        hint="notif-confirm-channel-hint"
        options=slug_options(&["0", "5", "10", "25", "50"], "notif-confirm-")
        value=Signal::derive(move || notes.get().confirm_channel_over.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(count) = value.parse::<u32>() {
            edit(Box::new(move |n| n.confirm_channel_over = count));
          }
        })
      />
      <KeywordRow
        id="notif-keywords"
        label="notif-keywords"
        hint="notif-keywords-hint"
        items=Signal::derive(move || notes.get().keywords)
        on_add=Callback::new(move |word: String| edit(Box::new(move |n| NotificationPrefs::add_keyword(&mut n.keywords, &word))))
        on_remove=Callback::new(move |index: usize| {
          edit(Box::new(move |n| {
            if index < n.keywords.len() {
              n.keywords.remove(index);
            }
          }))
        })
      />
      <KeywordRow
        id="notif-highlights"
        label="notif-highlights"
        hint="notif-highlights-hint"
        items=Signal::derive(move || notes.get().highlight_keywords)
        on_add=Callback::new(move |word: String| edit(Box::new(move |n| NotificationPrefs::add_keyword(&mut n.highlight_keywords, &word))))
        on_remove=Callback::new(move |index: usize| {
          edit(Box::new(move |n| {
            if index < n.highlight_keywords.len() {
              n.highlight_keywords.remove(index);
            }
          }))
        })
      />
    </SettingsCard>
    <SettingsCard heading="notif-threads">
      <SelectRow
        label="notif-thread-replies"
        options=slug_options(&["participating", "started", "mentions"], "notif-replies-")
        is_disabled=is_off
        value=Signal::derive(move || notes.get().thread_replies.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(replies) = ThreadReplies::from_slug(&slug) {
            edit(Box::new(move |n| n.thread_replies = replies));
          }
        })
      />
      <SwitchRow
        label="notif-auto-follow"
        value=Signal::derive(move || notes.get().auto_follow_on_channel_mention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.auto_follow_on_channel_mention = value)))
      />
    </SettingsCard>
    <SettingsCard heading="notif-phone" hint="notif-phone-hint">
      <SelectRow
        label="notif-push"
        options=level_options()
        is_disabled=is_off
        value=Signal::derive(move || notes.get().push.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(level) = NotifyLevel::from_slug(&slug) {
            edit(Box::new(move |n| n.push = level));
          }
        })
      />
      <SelectRow
        label="notif-push-when"
        options=slug_options(&["always", "away-or-offline"], "notif-push-when-")
        is_disabled=is_off
        value=Signal::derive(move || notes.get().push_when.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(when) = PushWhen::from_slug(&slug) {
            edit(Box::new(move |n| n.push_when = when));
          }
        })
      />
      <SelectRow
        label="notif-push-delay"
        hint="notif-push-delay-hint"
        options=slug_options(&["0", "1", "2", "5", "10"], "notif-delay-")
        is_disabled=is_off
        value=Signal::derive(move || notes.get().push_delay_minutes.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(minutes) = value.parse::<u32>() {
            edit(Box::new(move |n| n.push_delay_minutes = minutes));
          }
        })
      />
      <SwitchRow
        label="notif-uses-attention"
        hint="notif-uses-attention-hint"
        is_disabled=is_off
        value=Signal::derive(move || notes.get().uses_desktop_attention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.uses_desktop_attention = value)))
      />
      <SelectRow
        label="notif-attention-input"
        hint="notif-attention-input-hint"
        options=slug_options(&["60", "120", "300", "600"], "notif-attention-input-")
        is_disabled=Signal::derive(move || is_off.get() || !notes.get().uses_desktop_attention)
        value=Signal::derive(move || notes.get().attention_input_secs.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(seconds) = value.parse::<u32>() {
            edit(Box::new(move |n| n.attention_input_secs = seconds));
          }
        })
      />
    </SettingsCard>
    <SettingsCard heading="notif-calls-meetings">
      // Calls ring in the desktop app only; the browser has no call media.
      <Show when=tauri::is_available fallback=|| ()>
        <SwitchRow
          label="notif-ringtone"
          hint="notif-ringtone-hint"
          value=Signal::derive(move || notes.get().plays_ringtone)
          on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.plays_ringtone = value)))
        />
      </Show>
      <SelectRow
        label="notif-meeting-reminder"
        hint="notif-meeting-hint"
        options=slug_options(&["off", "1", "5", "10", "15"], "notif-meeting-reminder-")
        value=Signal::derive(move || {
          notes
            .get()
            .maybe_meeting_reminder_minutes
            .map_or_else(|| "off".to_owned(), |minutes| minutes.to_string())
        })
        on_change=Callback::new(move |value: String| {
          let maybe_minutes = value.parse::<u16>().ok();
          edit(Box::new(move |n| n.maybe_meeting_reminder_minutes = maybe_minutes));
        })
      />
      <SwitchRow
        label="notif-meeting-start"
        value=Signal::derive(move || notes.get().notifies_meeting_start)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.notifies_meeting_start = value)))
      />
    </SettingsCard>
    <DndCard />
    <PerConversationCard />
  }
}

/// Whether this device shows notifications, and a test notification to check it.
#[component]
fn ThisDeviceCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_desktop = tauri::is_available();
  let page_permission = RwSignal::new(tauri::notification_permission());
  let push_key = StoredValue::new(format!("teamcraft.web_push.{}", state.me().id));
  let is_push_on = RwSignal::new(load_string(&push_key.get_value()).is_some());
  let allowing = PendingAction::new();
  let testing = PendingAction::new();
  let has_tested = RwSignal::new(false);
  let is_granted = move || is_desktop || page_permission.get().as_deref() == Some("granted");

  // Read here, inside the reactive owner: the spawned future has no context to find it in.
  let maybe_session = crate::session::use_session();
  // Allowing notifications also subscribes to Web Push, so they arrive with no page open.
  let allow = move |_| {
    let push_key = push_key.get_value();
    allowing.spawn(async move {
      if page_permission.try_get_untracked().flatten().as_deref() != Some("granted") {
        let answer = tauri::request_notification_permission().await;
        page_permission.set_if_alive(answer);
      }
      if page_permission.try_get_untracked().flatten().as_deref() != Some("granted") {
        // The user said no in the browser's own prompt; the hint says how to change that.
        return Ok(());
      }
      let Some(session) = maybe_session else {
        return Err(ActionFailure::new(
          FailureKind::Other,
          "no session to subscribe to push with",
        ));
      };
      crate::remote_backend::enable_web_push(session.authenticator().as_ref())
        .await
        .map_err(|error| ActionFailure::new(FailureKind::Other, error))?;
      store_string(&push_key, "1");
      is_push_on.set_if_alive(true);
      Ok(())
    });
  };
  let send_test = move |_| {
    let notification = OsNotification {
      tag: NotificationTag::reminder(TEST_TAG),
      kind: OsNotificationKind::Reminder,
      title: i18n.t("notif-test-title"),
      maybe_body: Some(i18n.t("notif-test-body")),
      actions: Vec::new(),
      maybe_reply: None,
    };
    testing.spawn(async move {
      tauri::try_show_notification(&notification)
        .await
        .map_err(|error| ActionFailure::new(FailureKind::Other, error))?;
      has_tested.set_if_alive(true);
      Ok(())
    });
  };
  let status = Signal::derive(move || {
    let text = if is_desktop {
      i18n.t("notif-device-desktop")
    } else {
      match page_permission.get().as_deref() {
        Some("granted") if is_push_on.get() => i18n.t("notif-browser-background"),
        Some("granted") => i18n.t("notif-browser-granted"),
        Some("denied") => i18n.t("notif-browser-denied"),
        Some(_) => i18n.t("notif-browser-hint"),
        None => i18n.t("notif-browser-unsupported"),
      }
    };
    if has_tested.get() {
      format!("{text} {}", i18n.t("notif-test-sent"))
    } else {
      text
    }
  });

  view! {
    <SettingsCard heading="notif-this-device">
      <SettingRow label=if is_desktop { "notif-device-system" } else { "notif-browser" } live_hint=status>
        <Show when=move || !is_desktop && !is_push_on.get() && matches!(page_permission.get().as_deref(), Some("default") | Some("granted")) fallback=|| ()>
          <PendingButton pending=allowing class="btn btn-small btn-primary" on_click=allow>
            {move || i18n.t("notif-browser-allow")}
          </PendingButton>
        </Show>
        <Show when=is_granted fallback=|| ()>
          <span class="set-inline" id=row_id("notif-test")>
            <PendingButton pending=testing class="btn btn-small" on_click=send_test>
              <Icon name=IconName::Bell />
              <span>{move || i18n.t("notif-test")}</span>
            </PendingButton>
          </span>
        </Show>
      </SettingRow>
    </SettingsCard>
  }
}

/// A list of words with an input to add more and a remove button per word.
#[component]
fn KeywordRow(
  id: &'static str,
  label: &'static str,
  hint: &'static str,
  #[prop(into)] items: Signal<Vec<String>>,
  on_add: Callback<String>,
  on_remove: Callback<usize>,
) -> impl IntoView {
  let i18n = use_i18n();
  let draft = RwSignal::new(String::new());
  let add = move || {
    on_add.run(draft.get_untracked());
    draft.set(String::new());
  };
  view! {
    <SettingRow label=label hint=hint is_stacked=true>
      <div class="set-inline-form">
        <input
          id=id
          class="input grow"
          name="keyword"
          autocomplete="off"
          spellcheck="false"
          enterkeyhint="done"
          aria-label=move || i18n.t(label)
          placeholder=move || i18n.t("keyword-placeholder")
          prop:value=move || draft.get()
          on:input=move |ev| draft.set(event_target_value(&ev))
          on:keydown=move |ev| {
            if ev.key() == "Enter" {
              ev.prevent_default();
              add();
            }
          }
        />
        <button type="button" class="btn" disabled=move || draft.get().trim().is_empty() on:click=move |_| add()>
          {move || i18n.t("keyword-add")}
        </button>
      </div>
      <ul class="keyword-chips" aria-label=move || i18n.t(label)>
        {move || {
          items
            .get()
            .into_iter()
            .enumerate()
            .map(|(index, word)| {
              let remove_label = word.clone();
              view! {
                <li class="token-chip">
                  <span translate="no">{word}</span>
                  <button
                    type="button"
                    class="icon-btn icon-btn-small"
                    aria-label=move || i18n.t_with("keyword-remove", &[("word", &remove_label)])
                    on:click=move |_| on_remove.run(index)
                  >
                    <Icon name=IconName::Close />
                  </button>
                </li>
              }
            })
            .collect_view()
        }}
      </ul>
    </SettingRow>
  }
}

/// The do-not-disturb schedule: several weekly ranges with their own weekdays, evaluated in this
/// device's time zone, and who or what still gets through (also during a manual DND).
#[component]
fn DndCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let dnd = Memo::new(move |_| prefs.get().notifications.dnd);
  let edit =
    move |change: Box<dyn FnOnce(&mut DndSchedule)>| prefs.update(|all| change(&mut all.notifications.dnd));
  let zone = local_time_zone();
  let people = Memo::new(move |_| {
    let me = state.me().id;
    let mut users: Vec<User> = state.read(|backend| backend.users());
    users.retain(|user| user.id != me && !user.is_bot);
    users.sort_by_key(|user| user.display_name.to_lowercase());
    users
  });
  let chosen_person = RwSignal::new(String::new());
  let add_person = move |_| {
    let Ok(raw) = chosen_person.get_untracked().parse::<UserId>() else {
      return;
    };
    edit(Box::new(move |dnd| {
      if !dnd.allow_user_ids.contains(&raw) && dnd.allow_user_ids.len() < MAX_DND_ALLOWED_USERS {
        dnd.allow_user_ids.push(raw);
      }
    }));
    chosen_person.set(String::new());
  };
  let name_of = move |id: UserId| {
    people
      .get()
      .into_iter()
      .find(|user| user.id == id)
      .map_or_else(|| id.to_string(), |user| user.display_name)
  };
  let can_add_range = move || dnd.with(|dnd| dnd.windows.len() < MAX_DND_WINDOWS);
  let add_range = move |_| edit(Box::new(|dnd| dnd.windows.push(default_dnd_window())));
  let is_scheduled = Signal::derive(move || dnd.get().is_enabled);
  let zone_hint = Signal::derive(move || match &zone {
    Some(zone) => i18n.t_with("notif-dnd-zone", &[("zone", zone)]),
    None => i18n.t("notif-dnd-zone-unknown"),
  });

  view! {
    <SettingsCard heading="notif-dnd">
      <SwitchRow
        label="notif-dnd-enable"
        hint="notif-dnd-hint"
        value=is_scheduled
        on_change=Callback::new(move |value: bool| edit(Box::new(move |dnd| dnd.is_enabled = value)))
      />
      <Show when=move || is_scheduled.get() fallback=|| ()>
        <SettingRow label="notif-dnd-ranges" live_hint=zone_hint is_stacked=true>
          <ul class="set-ranges" aria-label=move || i18n.t("notif-dnd-ranges")>
            {move || {
              dnd
                .get()
                .windows
                .into_iter()
                .enumerate()
                .map(|(index, window)| view! { <DndRange index=index window=window /> })
                .collect_view()
            }}
          </ul>
          <div class="set-card-actions">
            <button
              type="button"
              class="btn btn-small"
              prop:disabled=move || !can_add_range()
              on:click=add_range
            >
              <Icon name=IconName::Plus />
              <span>{move || i18n.t("notif-dnd-add-range")}</span>
            </button>
          </div>
        </SettingRow>
      </Show>
      <SwitchRow
        label="notif-dnd-urgent"
        hint="notif-dnd-urgent-hint"
        value=Signal::derive(move || dnd.get().allows_urgent)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |dnd| dnd.allows_urgent = value)))
      />
      <SettingRow label="notif-dnd-allow" hint="notif-dnd-allow-hint" is_stacked=true>
        <div class="set-inline-form">
          <select
            id="dnd-allow-person"
            class="input grow"
            aria-label=move || i18n.t("notif-dnd-allow")
            prop:value=move || chosen_person.get()
            on:change=move |ev| chosen_person.set(event_target_value(&ev))
          >
            <option value="">{move || i18n.t("notif-dnd-allow-choose")}</option>
            {move || {
              let allowed = dnd.get().allow_user_ids;
              people
                .get()
                .into_iter()
                .filter(|user| !allowed.contains(&user.id))
                .map(|user| view! { <option value=user.id.to_string()>{user.display_name}</option> })
                .collect_view()
            }}
          </select>
          <button
            type="button"
            class="btn"
            disabled=move || { chosen_person.get().is_empty() || dnd.get().allow_user_ids.len() >= MAX_DND_ALLOWED_USERS }
            on:click=add_person
          >
            {move || i18n.t("notif-dnd-allow-add")}
          </button>
        </div>
        <ul class="keyword-chips" aria-label=move || i18n.t("notif-dnd-allow")>
          {move || {
            dnd
              .get()
              .allow_user_ids
              .into_iter()
              .map(|id| {
                let name = name_of(id);
                let remove_label = name.clone();
                view! {
                  <li class="token-chip">
                    <span translate="no">{name}</span>
                    <button
                      type="button"
                      class="icon-btn icon-btn-small"
                      aria-label=move || i18n.t_with("notif-dnd-allow-remove", &[("name", &remove_label)])
                      on:click=move |_| edit(Box::new(move |dnd| dnd.allow_user_ids.retain(|allowed| *allowed != id)))
                    >
                      <Icon name=IconName::Close />
                    </button>
                  </li>
                }
              })
              .collect_view()
          }}
        </ul>
      </SettingRow>
    </SettingsCard>
  }
}

/// One quiet time of the schedule: its weekdays, start and end.
#[component]
fn DndRange(index: usize, window: DndWindow) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let edit = move |change: Box<dyn FnOnce(&mut DndWindow)>| {
    prefs.update(|all| {
      if let Some(window) = all.notifications.dnd.windows.get_mut(index) {
        change(window);
      }
    });
  };
  let days = RwSignal::new(window.weekdays.clone());
  // The chips edit a copy; changes flow into the schedule.
  Effect::new(move |previous: Option<()>| {
    let chosen = days.get();
    if previous.is_some() {
      edit(Box::new(move |window| window.weekdays = chosen));
    }
  });
  let from_id = format!("dnd-from-{index}");
  let to_id = format!("dnd-to-{index}");
  let number = (index + 1).to_string();
  view! {
    <li class="set-range" aria-label=move || i18n.t_with("notif-dnd-range", &[("number", &number)])>
      <WeekdayChips days=days />
      <div class="set-range-times">
        <label for=from_id.clone()>{move || i18n.t("notif-dnd-from")}</label>
        <input
          id=from_id
          class="input"
          type="time"
          prop:value=format_minutes(window.start_minute)
          on:change=move |ev| {
            if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
              edit(Box::new(move |window| window.start_minute = minute));
            }
          }
        />
        <label for=to_id.clone()>{move || i18n.t("notif-dnd-to")}</label>
        <input
          id=to_id
          class="input"
          type="time"
          prop:value=format_minutes(window.end_minute)
          on:change=move |ev| {
            if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
              edit(Box::new(move |window| window.end_minute = minute));
            }
          }
        />
        <button
          type="button"
          class="icon-btn"
          aria-label=move || i18n.t("notif-dnd-remove-range")
          title=move || i18n.t("notif-dnd-remove-range")
          on:click=move |_| {
            prefs.update(|all| {
              if index < all.notifications.dnd.windows.len() {
                all.notifications.dnd.windows.remove(index);
              }
            })
          }
        >
          <Icon name=IconName::Trash />
        </button>
      </div>
      {(!is_valid_window(&window)).then(|| view! { <p class="field-error" role="alert">{move || i18n.t("notif-dnd-range-invalid")}</p> })}
    </li>
  }
}

/// Levels and mutes of single conversations, with a filter: big instances have many.
#[component]
fn PerConversationCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let filter = RwSignal::new(String::new());
  let conversations = Memo::new(move |_| {
    state.read(|backend| {
      let mut list: Vec<Conversation> = Vec::new();
      for space in backend.spaces() {
        list.extend(backend.space_conversations(space.id));
      }
      list.extend(backend.direct_conversations());
      list
    })
  });
  let matching = Memo::new(move |_| {
    let query = filter.get();
    conversations.with(|list| {
      list
        .iter()
        .filter(|conversation| matches_filter(&conversation.name, &query))
        .cloned()
        .collect::<Vec<_>>()
    })
  });
  let shown =
    Memo::new(move |_| matching.with(|list| list.iter().take(MAX_LIST_ENTRIES).cloned().collect::<Vec<_>>()));

  view! {
    <SettingsCard heading="notif-per-conversation" hint="notif-per-conversation-hint">
      <ListFilter label="notif-per-conversation-filter" query=filter matching=Signal::derive(move || matching.with(Vec::len)) />
      <ul class="set-conversations">
        <For each=move || shown.get() key=|conversation| conversation.id let:conversation>
          {
            let id = conversation.id.to_string();
            let level_id = id.clone();
            let mute_id = id.clone();
            let read_level = id.clone();
            let read_mute = id.clone();
            view! {
              <li class="set-conversation">
                <span class="set-conversation-name" translate="no">{conversation.name.clone()}</span>
                <select
                  class="input set-select"
                  aria-label=move || i18n.t("notif-level-col")
                  on:change=move |ev| {
                    let slug = event_target_value(&ev);
                    let level_id = level_id.clone();
                    prefs.update(|all| {
                      let mut entry = all.notifications.conversation(&level_id);
                      entry.maybe_level = NotifyLevel::from_slug(&slug);
                      all.notifications.set_conversation(&level_id, entry);
                    });
                  }
                >
                  <option value="default" selected=move || prefs.get().notifications.conversation(&read_level).maybe_level.is_none()>
                    {move || i18n.t("notif-level-default")}
                  </option>
                  {NotifyLevel::ALL
                    .into_iter()
                    .map(|level| {
                      let check_id = id.clone();
                      view! {
                        <option value=level.slug() selected=move || prefs.get().notifications.conversation(&check_id).maybe_level == Some(level)>
                          {move || i18n.t(&format!("notify-level-{}", level.slug()))}
                        </option>
                      }
                    })
                    .collect_view()}
                </select>
                <select
                  class="input set-select"
                  aria-label=move || i18n.t("notif-mute-col")
                  on:change=move |ev| {
                    let mute = MuteFor::from_slug(&event_target_value(&ev)).unwrap_or(MuteFor::Off);
                    let mute_id = mute_id.clone();
                    prefs.update(|all| {
                      let mut entry = all.notifications.conversation(&mute_id);
                      entry.mute = mute;
                      all.notifications.set_conversation(&mute_id, entry);
                    });
                  }
                >
                  {MuteFor::ALL
                    .into_iter()
                    .map(|mute| {
                      let check_id = read_mute.clone();
                      view! {
                        <option value=mute.slug() selected=move || prefs.get().notifications.conversation(&check_id).mute == mute>
                          {move || i18n.t(&format!("notif-mute-{}", mute.slug()))}
                        </option>
                      }
                    })
                    .collect_view()}
                </select>
              </li>
            }
          }
        </For>
        <Show when=move || shown.with(Vec::is_empty) fallback=|| ()>
          <li class="set-empty">{move || i18n.t("notif-per-conversation-none")}</li>
        </Show>
      </ul>
    </SettingsCard>
    <p class="set-note">{move || i18n.t("notif-enforcement-note")}</p>
  }
}

fn level_options() -> Vec<(String, String)> {
  slug_options(&["all", "mentions", "nothing"], "notify-level-")
}

/// The master switch is on while desktop or push notifies about anything.
fn is_enabled(notes: &NotificationPrefs) -> bool {
  notes.desktop != NotifyLevel::Nothing || notes.push != NotifyLevel::Nothing
}

/// Off silences desktop and push; on brings back the default level where it was off.
fn set_enabled(notes: &mut NotificationPrefs, is_on: bool) {
  let defaults = NotificationPrefs::default();
  for (level, default) in [
    (&mut notes.desktop, defaults.desktop),
    (&mut notes.push, defaults.push),
  ] {
    if !is_on {
      *level = NotifyLevel::Nothing;
    } else if *level == NotifyLevel::Nothing {
      *level = default;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_master_switch_silences_and_restores_both_channels() {
    let mut notes = NotificationPrefs {
      desktop: NotifyLevel::All,
      ..NotificationPrefs::default()
    };
    assert!(is_enabled(&notes));
    set_enabled(&mut notes, false);
    assert_eq!(
      (notes.desktop, notes.push),
      (NotifyLevel::Nothing, NotifyLevel::Nothing)
    );
    assert!(!is_enabled(&notes));
    set_enabled(&mut notes, true);
    let defaults = NotificationPrefs::default();
    assert_eq!((notes.desktop, notes.push), (defaults.desktop, defaults.push));
  }

  #[test]
  fn turning_on_keeps_a_level_that_was_chosen() {
    let mut notes = NotificationPrefs {
      desktop: NotifyLevel::All,
      push: NotifyLevel::Nothing,
      ..NotificationPrefs::default()
    };
    set_enabled(&mut notes, true);
    assert_eq!(notes.desktop, NotifyLevel::All);
    assert_eq!(notes.push, NotificationPrefs::default().push);
  }
}
