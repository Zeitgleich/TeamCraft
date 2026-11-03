//! Settings > Notifications: one master switch, this device's notification status with a test
//! notification, then what notifies the user, the sound for new messages, do not disturb and
//! per-conversation levels. The values live in `NotificationPrefs` (see `prefs.rs`); `notify_sync`
//! sends the server's share to the server.

use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::message_sound::{
  BuiltInSound, MessageSound, OwnSoundLimits, OwnSoundPick, OwnSoundRefusal, OwnSoundStatus,
};
use teamcraft_client_api::os_notify::{NotificationTag, OsNotification, OsNotificationKind};
use teamcraft_proto::DndWindow;

use crate::auth::tauri;
use crate::call_alerts::may_name_names;
use crate::clock::local_time_zone;
use crate::components::icons::{Icon, IconName};
use crate::components::notification_prompt;
use crate::components::pending_button::PendingButton;
use crate::components::people_picker::PeoplePicker;
use crate::components::settings_account::WeekdayChips;
use crate::components::settings_rows::{
  ListFilter, MAX_LIST_ENTRIES, SelectRow, SettingRow, SettingsCard, SwitchRow, matches_filter, row_id,
  slug_options,
};
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::message_sounds::MessageSounds;
use crate::model::{Conversation, User, UserId};
use crate::notification_pictures::picture_of;
use crate::notifier::PictureSubject;
use crate::notify_sync::{
  MAX_DND_ALLOWED_USERS, MAX_DND_WINDOWS, format_minutes, is_valid_window, parse_minutes,
};
use crate::prefs::{
  DndSchedule, MuteFor, NotificationPrefs, NotifyLevel, PushWhen, ThreadReplies, default_dnd_window,
};
use crate::reactive::{SetIfAlive, UpdateIfAlive};
use crate::state::use_app_state;

/// The tag of the test notification; a newer test replaces the last one.
const TEST_TAG: &str = "settings-test";
/// What the page's file picker offers for an own sound (the desktop shell's dialog has its own list).
const SOUND_FILE_TYPES: &str = "audio/*,.mp3,.ogg,.oga,.wav,.flac";

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
    <MessageSoundCard is_off=is_off />
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
        <SwitchRow
          label="notif-call-window"
          hint="notif-call-window-hint"
          value=Signal::derive(move || notes.get().shows_call_window)
          on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.shows_call_window = value)))
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
  let me = state.me().id;
  let is_push_on = RwSignal::new(notification_prompt::is_push_on(me));
  let allowing = PendingAction::new();
  let testing = PendingAction::new();
  let has_tested = RwSignal::new(false);
  // What the last click on "Turn on" came to, where the permission alone doesn't say it: no
  // prompt shown, or refused without one (a private window).
  let maybe_outcome = RwSignal::new(None::<notification_prompt::AllowOutcome>);
  let is_granted = move || is_desktop || page_permission.get().as_deref() == Some("granted");

  // Read here, inside the reactive owner: the spawned future has no context to find it in.
  let maybe_session = crate::session::use_session();
  // Allowing notifications also subscribes to Web Push, so they arrive with no page open.
  let allow = move |_| {
    // Started inside the click: the browser only prompts for a request made in this task (#893).
    let request = tauri::request_notification_permission();
    allowing.spawn(async move {
      let result = notification_prompt::allow_notifications(me, maybe_session, request).await;
      page_permission.set_if_alive(tauri::notification_permission());
      let outcome = result?;
      maybe_outcome.set_if_alive(Some(outcome));
      if outcome == notification_prompt::AllowOutcome::On {
        is_push_on.set_if_alive(true);
      }
      Ok(())
    });
  };
  let send_test = move |_| {
    // The user's own picture, as a direct message shows its sender's (#543), where names may show.
    let maybe_picture = state.read(|backend| {
      may_name_names(&backend.push_content_mode())
        .then(|| picture_of(backend, PictureSubject::Person(backend.me().id)))
        .flatten()
    });
    let notification = OsNotification {
      tag: NotificationTag::reminder(TEST_TAG),
      kind: OsNotificationKind::Reminder,
      title: i18n.t("notif-test-title"),
      maybe_body: Some(i18n.t("notif-test-body")),
      actions: Vec::new(),
      maybe_reply: None,
      maybe_picture,
      sound: Default::default(),
    };
    testing.spawn(async move {
      tauri::try_show_notification(&notification).await?;
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
        Some("denied") if maybe_outcome.get() == Some(notification_prompt::AllowOutcome::Refused) => {
          i18n.t("notif-prompt-refused")
        },
        Some("denied") => i18n.t("notif-browser-denied"),
        Some(_) if maybe_outcome.get() == Some(notification_prompt::AllowOutcome::NoPrompt) => {
          i18n.t("notif-prompt-no-prompt")
        },
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

  // The row is always in the DOM, so a search hit for the test lands on it, also without permission.
  view! {
    <SettingsCard heading="notif-this-device">
      <SettingRow label=if is_desktop { "notif-device-system" } else { "notif-browser" } live_hint=status>
        <Show when=move || !is_desktop && !is_push_on.get() && matches!(page_permission.get().as_deref(), Some("default") | Some("granted")) fallback=|| ()>
          <PendingButton pending=allowing class="btn btn-small btn-primary" on_click=allow>
            {move || i18n.t("notif-browser-allow")}
          </PendingButton>
        </Show>
        <span class="set-inline" id=row_id("notif-test")>
          <Show when=is_granted fallback=|| ()>
            <PendingButton pending=testing class="btn btn-small" on_click=send_test>
              <Icon name=IconName::Bell />
              <span>{move || i18n.t("notif-test")}</span>
            </PendingButton>
          </Show>
        </span>
      </SettingRow>
    </SettingsCard>
  }
}

/// The sound for new messages (#800): the choice with a preview, and with "Own file…" this
/// device's file, picked, checked and kept here only, and how long it may play.
#[component]
fn MessageSoundCard(#[prop(into)] is_off: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let sound = Memo::new(move |_| prefs.get().notifications.message_sound);
  let limits = Memo::new(move |_| prefs.get().notifications.own_sound_limits);
  let sounds = use_context::<MessageSounds>().unwrap_or_else(MessageSounds::provide);
  let picking = PendingAction::new();
  let removing = PendingAction::new();
  let maybe_refusal = RwSignal::new(None::<OwnSoundRefusal>);
  let is_desktop = tauri::is_available();
  let file_input = NodeRef::<leptos::html::Input>::new();
  let has_file = Signal::derive(move || matches!(sounds.own_status.get(), Some(OwnSoundStatus::Ready(_))));

  // The own file is looked at when the card shows (it may have gone since) and again when the
  // length limit changes.
  Effect::new(move |_| {
    let limits = limits.get();
    leptos::task::spawn_local(async move { sounds.refresh_status(limits).await });
  });

  // A kept file becomes the choice and plays once, as a preview.
  let after_pick = move |pick: OwnSoundPick, limits: OwnSoundLimits| match pick {
    OwnSoundPick::Stored(_) => {
      prefs.update_if_alive(|all| all.notifications.message_sound = MessageSound::OwnFile);
      sounds.preview(MessageSound::OwnFile, limits);
    },
    OwnSoundPick::Refused { reason } => maybe_refusal.set_if_alive(Some(reason)),
    OwnSoundPick::Cancelled => {},
  };
  let choose_file = move || {
    maybe_refusal.set(None);
    let limits = limits.get_untracked();
    if is_desktop {
      let filter_label = i18n.t("notif-own-sound-filter");
      picking.spawn(async move {
        let pick = sounds.pick_in_shell(limits, filter_label).await?;
        after_pick(pick, limits);
        Ok(())
      });
    } else if let Some(input) = file_input.get_untracked() {
      // Inside the click: browsers open a file picker only then.
      input.click();
    }
  };
  let on_file = move |ev: leptos::ev::Event| {
    #[cfg(target_arch = "wasm32")]
    {
      let input: web_sys::HtmlInputElement = event_target(&ev);
      let Some(file) = input.files().and_then(|files| files.get(0)) else {
        return;
      };
      // Cleared, so picking the same file again counts as a change too.
      input.set_value("");
      let limits = limits.get_untracked();
      picking.spawn(async move {
        let pick = sounds.keep_picked(file, limits).await?;
        after_pick(pick, limits);
        Ok(())
      });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ev, after_pick);
  };
  let on_choice = move |ev: leptos::ev::Event| {
    let chosen = MessageSound::from_slug(&event_target_value(&ev));
    prefs.update(|all| all.notifications.message_sound = chosen);
    maybe_refusal.set(None);
    // "Own file…" without a file opens the picker at once; any other sound plays as a preview.
    if chosen == MessageSound::OwnFile && !has_file.get_untracked() {
      choose_file();
    } else if chosen.is_played_by_app() {
      sounds.preview(chosen, limits.get_untracked());
    }
  };
  let remove = move |_| {
    maybe_refusal.set(None);
    removing.spawn(async move { sounds.remove().await });
  };
  let file_hint = Signal::derive(move || {
    let limits = limits.get();
    // A refusal says what to choose instead; the rules would only repeat it.
    if let Some(reason) = maybe_refusal.get() {
      return refusal_text(i18n, reason, limits);
    }
    let status = match sounds.own_status.get() {
      Some(OwnSoundStatus::Ready(info)) => i18n.t_with(
        "notif-own-sound-current",
        &[
          ("name", &info.file_name),
          ("seconds", &format!("{:.1}", f64::from(info.play_ms) / 1000.0)),
        ],
      ),
      Some(OwnSoundStatus::Unavailable) => i18n.t("notif-own-sound-unavailable"),
      Some(OwnSoundStatus::NotPicked) | None => i18n.t("notif-own-sound-none"),
    };
    let rules = i18n.t_with(
      "notif-own-sound-limits",
      &[
        ("size", &file_size(limits.max_bytes())),
        ("seconds", &limits.max_play().as_secs().to_string()),
      ],
    );
    format!("{status} {rules}")
  });
  let options: Vec<(String, &'static str)> = MessageSound::choices()
    .map(|choice| (choice.slug().to_owned(), sound_label_key(choice)))
    .collect();

  view! {
    <SettingsCard heading="notif-sound">
      <SettingRow label="notif-message-sound" hint="notif-message-sound-hint">
        <span class="set-sound-picker">
          <select
            class="input set-select"
            aria-labelledby=format!("{}-label", row_id("notif-message-sound"))
            aria-describedby=format!("{}-hint", row_id("notif-message-sound"))
            prop:disabled=move || is_off.get()
            on:change=on_choice
          >
            {options
              .into_iter()
              .map(|(slug, key)| {
                let selected_slug = slug.clone();
                view! {
                  <option value=slug selected=move || sound.get().slug() == selected_slug>
                    {move || i18n.t(key)}
                  </option>
                }
              })
              .collect_view()}
          </select>
          <button
            type="button"
            class="btn btn-small"
            prop:disabled=move || is_off.get() || !sound.get().is_played_by_app()
            on:click=move |_| sounds.preview(sound.get_untracked(), limits.get_untracked())
          >
            <Icon name=IconName::Play />
            <span>{move || i18n.t("notif-sound-play")}</span>
          </button>
        </span>
      </SettingRow>
      <Show when=move || sound.get() == MessageSound::OwnFile fallback=|| ()>
        <SettingRow label="notif-own-sound" live_hint=file_hint>
          <PendingButton pending=picking class="btn btn-small" on_click=move |_| choose_file()>
            {move || if has_file.get() { i18n.t("notif-own-sound-replace") } else { i18n.t("notif-own-sound-choose") }}
          </PendingButton>
          <Show when=move || has_file.get() || sounds.own_status.get() == Some(OwnSoundStatus::Unavailable) fallback=|| ()>
            <PendingButton pending=removing class="btn btn-small" on_click=remove>
              {move || i18n.t("notif-own-sound-remove")}
            </PendingButton>
          </Show>
          {(!is_desktop).then(|| view! {
            <input type="file" hidden=true accept=SOUND_FILE_TYPES node_ref=file_input on:change=on_file />
          })}
        </SettingRow>
        <SelectRow
          label="notif-own-sound-length"
          options=slug_options(&["2", "5", "10"], "notif-own-sound-length-")
          value=Signal::derive(move || limits.get().max_play_seconds.to_string())
          on_change=Callback::new(move |value: String| {
            if let Ok(seconds) = value.parse::<u32>() {
              prefs.update(|all| all.notifications.own_sound_limits.max_play_seconds = seconds);
            }
          })
        />
      </Show>
    </SettingsCard>
  }
}

/// The i18n key naming a choice in the list.
fn sound_label_key(sound: MessageSound) -> &'static str {
  match sound {
    MessageSound::System => "notif-sound-system",
    MessageSound::Silent => "notif-sound-none",
    MessageSound::OwnFile => "notif-sound-own",
    MessageSound::BuiltIn(BuiltInSound::Chime) => "notif-sound-chime",
    MessageSound::BuiltIn(BuiltInSound::Ping) => "notif-sound-ping",
    MessageSound::BuiltIn(BuiltInSound::Pop) => "notif-sound-pop",
    MessageSound::BuiltIn(BuiltInSound::Marimba) => "notif-sound-marimba",
    MessageSound::BuiltIn(BuiltInSound::Knock) => "notif-sound-knock",
    MessageSound::BuiltIn(BuiltInSound::Keys) => "notif-sound-keys",
    MessageSound::BuiltIn(BuiltInSound::UhOh) => "notif-sound-uh-oh",
  }
}

/// Why a picked file was not kept, and what to do instead.
fn refusal_text(i18n: I18n, reason: OwnSoundRefusal, limits: OwnSoundLimits) -> String {
  match reason {
    OwnSoundRefusal::TooLarge => i18n.t_with(
      "notif-own-sound-too-large",
      &[("size", &file_size(limits.max_bytes()))],
    ),
    OwnSoundRefusal::NotAudio => i18n.t("notif-own-sound-not-audio"),
    OwnSoundRefusal::Unreadable | OwnSoundRefusal::Unknown => i18n.t("notif-own-sound-unreadable"),
  }
}

/// A size limit as people read it: whole megabytes without a decimal ("1 MB"), anything else as
/// the file views show sizes.
fn file_size(bytes: u64) -> String {
  const MIB: u64 = 1024 * 1024;
  if bytes >= MIB && bytes.is_multiple_of(MIB) {
    format!("{} MB", bytes / MIB)
  } else {
    format_bytes(bytes)
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
  // The shared people picker (search, paged results from the directory) in place of a `<select>`
  // with every user of the instance (#430). A person picked there goes into the allowed list at
  // once, which the chips below show and edit; the picker itself starts empty again.
  let picked = RwSignal::new(Vec::<User>::new());
  Effect::new(move |_| {
    let people = picked.get();
    if people.is_empty() {
      return;
    }
    let ids: Vec<UserId> = people.iter().map(|user| user.id).collect();
    edit(Box::new(move |dnd| {
      allow_people(&mut dnd.allow_user_ids, &ids, MAX_DND_ALLOWED_USERS)
    }));
    picked.set(Vec::new());
  });
  let allowed_ids =
    Signal::derive(move || dnd.with(|dnd| dnd.allow_user_ids.iter().copied().collect::<HashSet<UserId>>()));
  let is_allow_list_full = move || dnd.with(|dnd| dnd.allow_user_ids.len() >= MAX_DND_ALLOWED_USERS);
  let name_of = move |id: UserId| {
    state
      .read(|backend| backend.user(id))
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
        <Show
          when=move || !is_allow_list_full()
          fallback=move || view! { <p class="muted-text">{move || i18n.t_with("notif-dnd-allow-full", &[("max", &MAX_DND_ALLOWED_USERS.to_string())])}</p> }
        >
          <PeoplePicker
            chosen=picked
            chosen_label_key="notif-dnd-allow"
            allows_guest_accounts=true
            already_in=allowed_ids
            already_in_key="notif-dnd-allow-already"
          />
        </Show>
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

/// Adds `people` to the people who break through do not disturb: each only once, and never more
/// than `max` altogether.
fn allow_people(allowed: &mut Vec<UserId>, people: &[UserId], max: usize) {
  for person in people {
    if allowed.len() >= max {
      break;
    }
    if !allowed.contains(person) {
      allowed.push(*person);
    }
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
  use crate::model::id_from_raw;

  #[test]
  fn people_are_allowed_once_and_never_beyond_the_limit() {
    let person = |raw| id_from_raw::<UserId>(raw);
    let mut allowed = vec![person(1)];

    allow_people(&mut allowed, &[person(2), person(1), person(3)], 3);
    assert_eq!(
      allowed,
      [person(1), person(2), person(3)],
      "no duplicate, in the order picked"
    );

    allow_people(&mut allowed, &[person(4)], 3);
    assert_eq!(allowed.len(), 3, "the limit holds");
  }

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

  #[test]
  fn size_limits_read_as_whole_megabytes_where_they_are() {
    assert_eq!(file_size(1024 * 1024), "1 MB");
    assert_eq!(file_size(16 * 1024 * 1024), "16 MB");
    assert_eq!(file_size(512 * 1024), "512.0 KB");
  }

  #[test]
  fn every_sound_has_a_name_in_each_language() {
    let mut keys: Vec<&str> = MessageSound::choices().map(sound_label_key).collect();
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De] {
      for key in &keys {
        assert_ne!(
          &crate::i18n::translate(locale, key, None),
          key,
          "{locale:?} lacks {key}"
        );
      }
    }
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), MessageSound::choices().count());
  }
}
