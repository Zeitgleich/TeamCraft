//! The tabs of the settings dialog. Preferences are edited in `Settings::prefs`; see `prefs.rs` for
//! what each value means and which ones are enforced yet.

use leptos::prelude::*;

use crate::clock::{format_relative, now_ms};
use crate::components::background_picker::{BackgroundPicker, effect_from_slug, effect_slug};
use crate::components::icons::{Icon, IconName};
use teamcraft_client_api::privacy::LastSeenVisibility;

use crate::clock::local_time_zone;
use crate::i18n::{Locale, use_i18n};
use crate::keys::t_with_shortcuts;
use crate::model::User;
use crate::model::{Conversation, DeviceId, UserId};
use crate::notify_sync::{
  MAX_DND_ALLOWED_USERS, MAX_DND_WINDOWS, format_minutes, is_valid_window, parse_minutes,
};
use crate::prefs::{
  CallPrefs, Density, DndSchedule, EVERY_DAY, MuteFor, NotificationPrefs, NotifyLevel, PushWhen, SidebarSort,
  ThreadReplies, Zoom, default_dnd_window,
};
use crate::settings::{SendMode, Theme, load_string, store_string};
use crate::state::{Overlay, use_app_state};
use teamcraft_proto::DndWindow;

/// A labelled `<select>` bound to a stored slug.
#[component]
fn SelectField(
  id: &'static str,
  /// i18n key of the label.
  label: &'static str,
  /// (slug, i18n key) pairs.
  options: Vec<(String, String)>,
  #[prop(into)] value: Signal<String>,
  on_change: Callback<String>,
  /// i18n key of a hint below the field.
  #[prop(optional)]
  hint: Option<&'static str>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <select id=id class="input" on:change=move |ev| on_change.run(event_target_value(&ev))>
        {options
          .into_iter()
          .map(|(slug, key)| {
            let selected_slug = slug.clone();
            view! {
              <option value=slug selected=move || value.get() == selected_slug>
                {move || i18n.t(&key)}
              </option>
            }
          })
          .collect_view()}
      </select>
      {hint.map(|key| view! { <p class="muted-text small">{move || i18n.t(key)}</p> })}
    </div>
  }
}

/// A checkbox with its label.
#[component]
fn CheckField(
  label: &'static str,
  #[prop(into)] value: Signal<bool>,
  on_change: Callback<bool>,
  #[prop(optional)] hint: Option<&'static str>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <label class="checkbox settings-check">
      <input
        type="checkbox"
        prop:checked=move || value.get()
        on:change=move |ev| on_change.run(event_target_checked(&ev))
      />
      <span>
        {move || i18n.t(label)}
        {hint.map(|key| view! { <small class="muted-text">{move || i18n.t(key)}</small> })}
      </span>
    </label>
  }
}

fn options(slugs: &[&str], prefix: &str) -> Vec<(String, String)> {
  slugs
    .iter()
    .map(|slug| ((*slug).to_owned(), format!("{prefix}{slug}")))
    .collect()
}

/// A list of words with an input to add more and a remove button per word.
#[component]
fn KeywordField(
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
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <div class="field-row">
        <input
          id=id
          class="input grow"
          name="keyword"
          autocomplete="off"
          spellcheck="false"
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
      <p class="muted-text small">{move || i18n.t(hint)}</p>
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
    </div>
  }
}

fn level_options() -> Vec<(String, String)> {
  options(&["all", "mentions", "nothing"], "notify-level-")
}

#[component]
pub fn NotificationsTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let notes = Memo::new(move |_| prefs.get().notifications);
  let edit =
    move |change: Box<dyn FnOnce(&mut NotificationPrefs)>| prefs.update(|all| change(&mut all.notifications));

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

  let page_permission = RwSignal::new(crate::auth::tauri::notification_permission());
  let push_key = StoredValue::new(format!("teamcraft.web_push.{}", state.me().id));
  let is_push_on = RwSignal::new(load_string(&push_key.get_value()).is_some());
  let push_error = RwSignal::new(None::<String>);
  // Allowing notifications also subscribes to Web Push, so they arrive with no page open.
  let allow_notifications = move |_| {
    let push_key = push_key.get_value();
    leptos::task::spawn_local(async move {
      if page_permission.get_untracked().as_deref() != Some("granted") {
        page_permission.set(crate::auth::tauri::request_notification_permission().await);
      }
      if page_permission.get_untracked().as_deref() != Some("granted") {
        return;
      }
      let Some(session) = crate::session::use_session() else {
        return;
      };
      match crate::remote_backend::enable_web_push(session.authenticator().as_ref()).await {
        Ok(()) => {
          store_string(&push_key, "1");
          is_push_on.set(true);
          push_error.set(None);
        },
        Err(error) => push_error.set(Some(error)),
      }
    });
  };

  view! {
    // The desktop app notifies through the system; the browser needs the user's permission.
    <Show when=move || !crate::auth::tauri::is_available() && page_permission.get().is_some() fallback=|| ()>
      <fieldset class="settings-group">
        <legend>{move || i18n.t("notif-browser")}</legend>
        <p class="muted-text small">
          {move || match page_permission.get().as_deref() {
            Some("granted") => i18n.t("notif-browser-granted"),
            Some("denied") => i18n.t("notif-browser-denied"),
            _ => i18n.t("notif-browser-hint"),
          }}
        </p>
        <Show when=move || !is_push_on.get() && page_permission.get().as_deref() != Some("denied") fallback=|| ()>
          <button class="btn" on:click=allow_notifications>{move || i18n.t("notif-browser-allow")}</button>
        </Show>
        <Show when=move || is_push_on.get() fallback=|| ()>
          <p class="muted-text small">{move || i18n.t("notif-browser-background")}</p>
        </Show>
        {move || push_error.get().map(|error| view! { <p class="field-error" role="alert">{error}</p> })}
      </fieldset>
    </Show>
    <fieldset class="settings-group">
      <legend>{move || i18n.t("notif-what")}</legend>
      <SelectField
        id="notif-desktop"
        label="notif-desktop"
        options=level_options()
        value=Signal::derive(move || notes.get().desktop.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(level) = NotifyLevel::from_slug(&slug) {
            edit(Box::new(move |n| n.desktop = level));
          }
        })
        hint="notif-level-hint"
      />
      <SelectField
        id="notif-push"
        label="notif-push"
        options=level_options()
        value=Signal::derive(move || notes.get().push.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(level) = NotifyLevel::from_slug(&slug) {
            edit(Box::new(move |n| n.push = level));
          }
        })
      />
      <SelectField
        id="notif-push-when"
        label="notif-push-when"
        options=options(&["always", "away-or-offline"], "notif-push-when-")
        value=Signal::derive(move || notes.get().push_when.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(when) = PushWhen::from_slug(&slug) {
            edit(Box::new(move |n| n.push_when = when));
          }
        })
      />
      <SelectField
        id="notif-push-delay"
        label="notif-push-delay"
        options=options(&["0", "1", "2", "5", "10"], "notif-delay-")
        value=Signal::derive(move || notes.get().push_delay_minutes.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(minutes) = value.parse::<u32>() {
            edit(Box::new(move |n| n.push_delay_minutes = minutes));
          }
        })
        hint="notif-push-delay-hint"
      />
      <SelectField
        id="notif-attention-input"
        label="notif-attention-input"
        options=options(&["60", "120", "300", "600"], "notif-attention-input-")
        value=Signal::derive(move || notes.get().attention_input_secs.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(seconds) = value.parse::<u32>() {
            edit(Box::new(move |n| n.attention_input_secs = seconds));
          }
        })
        hint="notif-attention-input-hint"
      />
      <CheckField
        label="notif-uses-attention"
        hint="notif-uses-attention-hint"
        value=Signal::derive(move || notes.get().uses_desktop_attention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.uses_desktop_attention = value)))
      />
    </fieldset>

    <fieldset class="settings-group">
      <legend>{move || i18n.t("notif-mentions")}</legend>
      <CheckField
        label="notif-first-name"
        value=Signal::derive(move || notes.get().first_name_is_mention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.first_name_is_mention = value)))
      />
      <CheckField
        label="notif-preview"
        hint="notif-preview-hint"
        value=Signal::derive(move || notes.get().show_preview)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.show_preview = value)))
      />
      <CheckField
        label="notif-channel-mentions"
        value=Signal::derive(move || notes.get().channel_mentions_notify)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.channel_mentions_notify = value)))
      />
      <SelectField
        id="notif-confirm-channel"
        label="notif-confirm-channel"
        options=options(&["0", "5", "10", "25", "50"], "notif-confirm-")
        value=Signal::derive(move || notes.get().confirm_channel_over.to_string())
        on_change=Callback::new(move |value: String| {
          if let Ok(count) = value.parse::<u32>() {
            edit(Box::new(move |n| n.confirm_channel_over = count));
          }
        })
      />
      <KeywordField
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
      <KeywordField
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
    </fieldset>

    <fieldset class="settings-group">
      <legend>{move || i18n.t("notif-threads")}</legend>
      <SelectField
        id="notif-thread-replies"
        label="notif-thread-replies"
        options=options(&["participating", "started", "mentions"], "notif-replies-")
        value=Signal::derive(move || notes.get().thread_replies.slug().to_owned())
        on_change=Callback::new(move |slug: String| {
          if let Some(replies) = ThreadReplies::from_slug(&slug) {
            edit(Box::new(move |n| n.thread_replies = replies));
          }
        })
      />
      <CheckField
        label="notif-auto-follow"
        value=Signal::derive(move || notes.get().auto_follow_on_channel_mention)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |n| n.auto_follow_on_channel_mention = value)))
      />
    </fieldset>

    <DndEditor />

    <fieldset class="settings-group">
      <legend>{move || i18n.t("notif-per-conversation")}</legend>
      <p class="muted-text small">{move || i18n.t("notif-per-conversation-hint")}</p>
      <div class="table-wrap conversation-notify">
        <table class="data-table">
          <caption class="visually-hidden">{move || i18n.t("notif-per-conversation")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("admin-col-channel")}</th>
              <th scope="col">{move || i18n.t("notif-level-col")}</th>
              <th scope="col">{move || i18n.t("notif-mute-col")}</th>
            </tr>
          </thead>
          <tbody>
            <For each=move || conversations.get() key=|conversation| conversation.id let:conversation>
              {
                let id = conversation.id.to_string();
                let level_id = id.clone();
                let mute_id = id.clone();
                let read_id = id.clone();
                let read_mute = id.clone();
                view! {
                  <tr>
                    <th scope="row" translate="no">{conversation.name.clone()}</th>
                    <td>
                      <label class="visually-hidden" for=format!("level-{id}")>{move || i18n.t("notif-level-col")}</label>
                      <select
                        id=format!("level-{id}")
                        class="input"
                        on:change=move |ev| {
                          let slug = event_target_value(&ev);
                          let level_id = level_id.clone();
                          prefs
                            .update(|all| {
                              let mut entry = all.notifications.conversation(&level_id);
                              entry.maybe_level = NotifyLevel::from_slug(&slug);
                              all.notifications.set_conversation(&level_id, entry);
                            });
                        }
                      >
                        <option value="default" selected=move || prefs.get().notifications.conversation(&read_id).maybe_level.is_none()>
                          {move || i18n.t("notif-level-default")}
                        </option>
                        {NotifyLevel::ALL
                          .into_iter()
                          .map(|level| {
                            let check_id = id.clone();
                            view! {
                              <option
                                value=level.slug()
                                selected=move || prefs.get().notifications.conversation(&check_id).maybe_level == Some(level)
                              >
                                {move || i18n.t(&format!("notify-level-{}", level.slug()))}
                              </option>
                            }
                          })
                          .collect_view()}
                      </select>
                    </td>
                    <td>
                      <label class="visually-hidden" for=format!("mute-{id}")>{move || i18n.t("notif-mute-col")}</label>
                      <select
                        id=format!("mute-{id}")
                        class="input"
                        on:change=move |ev| {
                          let mute = MuteFor::from_slug(&event_target_value(&ev)).unwrap_or(MuteFor::Off);
                          let mute_id = mute_id.clone();
                          prefs
                            .update(|all| {
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
                              <option
                                value=mute.slug()
                                selected=move || prefs.get().notifications.conversation(&check_id).mute == mute
                              >
                                {move || i18n.t(&format!("notif-mute-{}", mute.slug()))}
                              </option>
                            }
                          })
                          .collect_view()}
                      </select>
                    </td>
                  </tr>
                }
              }
            </For>
          </tbody>
        </table>
      </div>
    </fieldset>
    <p class="muted-text small">{move || i18n.t("notif-enforcement-note")}</p>
  }
}

/// The do-not-disturb schedule: several weekly ranges with their own weekdays, evaluated in this
/// device's time zone, and who or what still gets through (also during a manual DND).
#[component]
fn DndEditor() -> impl IntoView {
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

  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("notif-dnd")}</legend>
      <CheckField
        label="notif-dnd-enable"
        value=Signal::derive(move || dnd.get().is_enabled)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |dnd| dnd.is_enabled = value)))
        hint="notif-dnd-hint"
      />
      <p class="muted-text small">
        {move || match &zone {
          Some(zone) => i18n.t_with("notif-dnd-zone", &[("zone", zone)]),
          None => i18n.t("notif-dnd-zone-unknown"),
        }}
      </p>
      <ul class="dnd-ranges" aria-label=move || i18n.t("notif-dnd-ranges")>
        {move || {
          dnd
            .get()
            .windows
            .into_iter()
            .enumerate()
            .map(|(index, window)| view! { <DndRangeRow index=index window=window is_enabled=Signal::derive(move || dnd.get().is_enabled) /> })
            .collect_view()
        }}
      </ul>
      <button
        type="button"
        class="btn"
        disabled=move || { !dnd.get().is_enabled || dnd.get().windows.len() >= MAX_DND_WINDOWS }
        on:click=move |_| edit(Box::new(|dnd| dnd.windows.push(default_dnd_window())))
      >
        {move || i18n.t("notif-dnd-add-range")}
      </button>

      <CheckField
        label="notif-dnd-urgent"
        hint="notif-dnd-urgent-hint"
        value=Signal::derive(move || dnd.get().allows_urgent)
        on_change=Callback::new(move |value: bool| edit(Box::new(move |dnd| dnd.allows_urgent = value)))
      />
      <div class="field">
        <label for="dnd-allow-person">{move || i18n.t("notif-dnd-allow")}</label>
        <div class="field-row">
          <select
            id="dnd-allow-person"
            class="input grow"
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
        <p class="muted-text small">{move || i18n.t("notif-dnd-allow-hint")}</p>
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
      </div>
    </fieldset>
  }
}

/// One range of the schedule: its weekdays, start and end.
#[component]
fn DndRangeRow(index: usize, window: DndWindow, #[prop(into)] is_enabled: Signal<bool>) -> impl IntoView {
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
  let from_id = format!("dnd-from-{index}");
  let to_id = format!("dnd-to-{index}");
  let weekdays = window.weekdays.clone();
  view! {
    <li class="dnd-range">
      <div class="dnd-weekdays" role="group" aria-label=move || i18n.t_with("notif-dnd-range", &[("number", &(index + 1).to_string())])>
        {EVERY_DAY.iter()
          .map(|day| {
            let is_on = weekdays.contains(day);
            let key = format!("weekday-{}", day.as_str());
            let toggled = day.clone();
            view! {
              <label class="checkbox dnd-weekday">
                <input
                  type="checkbox"
                  prop:checked=is_on
                  prop:disabled=move || !is_enabled.get()
                  on:change=move |ev| {
                    let is_checked = event_target_checked(&ev);
                    let day = toggled.clone();
                    edit(Box::new(move |window| {
                      window.weekdays.retain(|existing| *existing != day);
                      if is_checked {
                        window.weekdays.push(day);
                      }
                    }));
                  }
                />
                <span>{move || i18n.t(&key)}</span>
              </label>
            }
          })
          .collect_view()}
      </div>
      <div class="field-row dnd-times">
        <div class="field">
          <label for=from_id.clone()>{move || i18n.t("notif-dnd-from")}</label>
          <input
            id=from_id
            class="input"
            type="time"
            prop:disabled=move || !is_enabled.get()
            prop:value=format_minutes(window.start_minute)
            on:change=move |ev| {
              if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
                edit(Box::new(move |window| window.start_minute = minute));
              }
            }
          />
        </div>
        <div class="field">
          <label for=to_id.clone()>{move || i18n.t("notif-dnd-to")}</label>
          <input
            id=to_id
            class="input"
            type="time"
            prop:disabled=move || !is_enabled.get()
            prop:value=format_minutes(window.end_minute)
            on:change=move |ev| {
              if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
                edit(Box::new(move |window| window.end_minute = minute));
              }
            }
          />
        </div>
        <button
          type="button"
          class="icon-btn"
          prop:disabled=move || !is_enabled.get()
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
          <Icon name=IconName::Close />
        </button>
      </div>
      {(!is_valid_window(&window)).then(|| view! { <p class="muted-text small" role="alert">{move || i18n.t("notif-dnd-range-invalid")}</p> })}
    </li>
  }
}

#[component]
pub fn DisplayTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;
  let prefs = settings.prefs;

  view! {
    <SelectField
      id="setting-theme"
      label="settings-theme"
      options=options(&["system", "light", "dark"], "settings-theme-")
      value=Signal::derive(move || settings.theme.get().slug().to_owned())
      on_change=Callback::new(move |slug: String| {
        if let Some(theme) = Theme::from_slug(&slug) {
          settings.theme.set(theme);
        }
      })
    />
    <SelectField
      id="setting-density"
      label="settings-density"
      options=options(&["comfortable", "compact"], "settings-density-")
      value=Signal::derive(move || prefs.get().display.density.slug().to_owned())
      on_change=Callback::new(move |slug: String| {
        if let Some(density) = Density::from_slug(&slug) {
          prefs.update(|all| all.display.density = density);
        }
      })
    />
    // Desktop app only: browsers zoom with their own Ctrl/Cmd + +/-/0.
    {crate::auth::tauri::is_available().then(|| view! {
    <div class="field">
      <label for="setting-zoom">{move || i18n.t("settings-zoom")}</label>
      <select
        id="setting-zoom"
        class="input"
        aria-describedby="setting-zoom-hint"
        on:change=move |ev| {
          if let Ok(percent) = event_target_value(&ev).parse::<u16>() {
            prefs.update(|all| all.display.zoom = Zoom::from_percent(percent));
          }
        }
      >
        {Zoom::STEPS
          .into_iter()
          .map(|percent| {
            view! {
              <option value=percent.to_string() selected=move || prefs.get().display.zoom.percent() == percent>
                {move || i18n.t_with("settings-zoom-percent", &[("percent", &percent.to_string())])}
              </option>
            }
          })
          .collect_view()}
      </select>
      <p class="muted-text small" id="setting-zoom-hint">{move || t_with_shortcuts(i18n, "settings-zoom-hint")}</p>
    </div>
    })}
    <fieldset class="settings-group">
      <legend>{move || i18n.t("settings-composer")}</legend>
      <label class="check-row">
        <input
          type="checkbox"
          prop:checked=move || state.settings.prefs.get().composer.convert_emoticons
          on:change=move |ev| {
            let on = event_target_checked(&ev);
            state.settings.prefs.update(|prefs| prefs.composer.convert_emoticons = on);
          }
        />
        <span>{move || i18n.t("settings-convert-emoticons")}</span>
      </label>
      <label class="check-row">
        <input
          type="checkbox"
          prop:checked=move || state.settings.prefs.get().composer.show_formatting
          on:change=move |ev| {
            let on = event_target_checked(&ev);
            state.settings.prefs.update(|prefs| prefs.composer.show_formatting = on);
          }
        />
        <span>{move || i18n.t("settings-show-formatting")}</span>
      </label>
    </fieldset>
    <fieldset class="field">
      <legend>{move || i18n.t("settings-send-mode")}</legend>
      {SendMode::ALL
        .into_iter()
        .map(|mode| {
          view! {
            <label class="radio">
              <input
                type="radio"
                name="send-mode"
                prop:checked=move || settings.send_mode.get() == mode
                on:change=move |_| settings.send_mode.set(mode)
              />
              <span>{move || t_with_shortcuts(i18n, &format!("settings-send-{}", mode.slug()))}</span>
            </label>
          }
        })
        .collect_view()}
    </fieldset>
    <CheckField
      label="settings-join-leave"
      value=settings.show_join_leave
      on_change=Callback::new(move |value: bool| settings.show_join_leave.set(value))
    />
    <CheckField
      label="settings-status-in-messages"
      value=Signal::derive(move || prefs.get().display.show_status_in_messages)
      on_change=Callback::new(move |value: bool| prefs.update(|all| all.display.show_status_in_messages = value))
    />
  }
}

#[component]
pub fn SidebarTab() -> impl IntoView {
  let state = use_app_state();
  let settings = state.settings;
  let prefs = settings.prefs;

  view! {
    <CheckField
      label="settings-unreads-group"
      value=settings.show_unreads_group
      on_change=Callback::new(move |value: bool| settings.show_unreads_group.set(value))
    />
    <SelectField
      id="setting-sidebar-sort"
      label="settings-sidebar-sort"
      options=options(&["default", "alphabetical"], "settings-sort-")
      value=Signal::derive(move || prefs.get().sidebar.sort.slug().to_owned())
      on_change=Callback::new(move |slug: String| {
        if let Some(sort) = SidebarSort::from_slug(&slug) {
          prefs.update(|all| all.sidebar.sort = sort);
        }
      })
    />
    <CheckField
      label="settings-sidebar-grouped"
      hint="settings-sidebar-grouped-hint"
      value=Signal::derive(move || prefs.get().sidebar.is_grouped_by_space)
      on_change=Callback::new(move |value: bool| prefs.update(|all| all.sidebar.is_grouped_by_space = value))
    />
  }
}

#[component]
pub fn LanguageTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;
  let language_options: Vec<(String, String)> = Locale::ALL
    .into_iter()
    .map(|locale| (locale.tag().to_owned(), format!("language-{}", locale.tag())))
    .collect();

  view! {
    <SelectField
      id="setting-language"
      label="settings-language"
      options=language_options
      value=Signal::derive(move || settings.locale.get().tag().to_owned())
      on_change=Callback::new(move |tag: String| {
        if let Some(locale) = Locale::from_tag(&tag) {
          settings.locale.set(locale);
        }
      })
      hint="settings-language-hint"
    />
    <p class="muted-text small">{move || i18n.t("settings-language-translate")}</p>
  }
}

#[component]
pub fn PrivacyTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let privacy = Memo::new(move |_| prefs.get().privacy);
  // Who may see when the user was last active lives on the server, not in the local preferences.
  let last_seen_choice = Memo::new(move |_| state.read(|backend| backend.privacy()));

  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("privacy-share")}</legend>
      <CheckField
        label="privacy-read-receipts"
        hint="privacy-read-receipts-hint"
        value=Signal::derive(move || privacy.get().share_read_receipts)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.privacy.share_read_receipts = value))
      />
      <CheckField
        label="privacy-typing"
        value=Signal::derive(move || privacy.get().share_typing)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.privacy.share_typing = value))
      />
      <CheckField
        label="privacy-last-seen"
        hint="privacy-last-seen-hint"
        value=Signal::derive(move || last_seen_choice.get().last_seen == LastSeenVisibility::Everyone)
        on_change=Callback::new(move |value: bool| {
          state.write(|backend| {
            backend.set_last_seen_visibility(if value {
              LastSeenVisibility::Everyone
            } else {
              LastSeenVisibility::Nobody
            })
          })
        })
      />
      <Show when=move || !last_seen_choice.get().is_last_seen_available fallback=|| ()>
        <p class="muted-text small">{move || i18n.t("privacy-last-seen-off")}</p>
      </Show>
    </fieldset>
    <SelectField
      id="privacy-away"
      label="privacy-away"
      options=options(&["5", "10", "15", "30", "never"], "privacy-away-")
      value=Signal::derive(move || privacy.get().away_after_minutes.map_or_else(|| "never".to_owned(), |minutes| minutes.to_string()))
      on_change=Callback::new(move |value: String| prefs.update(|all| all.privacy.away_after_minutes = value.parse::<u32>().ok()))
      hint="privacy-away-hint"
    />
    <p class="muted-text small">{move || i18n.t("privacy-offline-hint")}</p>
  }
}

/// Own devices, signing out a device, linking a new one, and verifying contacts.
#[component]
pub fn DevicesTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The list comes from the server: ask again whenever the tab opens.
  state.read(|backend| backend.refresh_my_devices());
  let devices = Memo::new(move |_| state.read(|backend| backend.my_devices()));
  let pending = RwSignal::new(None::<DeviceId>);
  let now = now_ms();
  let fingerprint = Memo::new(move |_| {
    crate::security::group_fingerprint(&state.read(|backend| backend.my_identity_fingerprint()))
  });
  let contacts = Memo::new(move |_| {
    let me = state.me().id;
    state.read(|backend| {
      backend
        .users()
        .into_iter()
        .filter(|user| user.id != me && !user.is_bot)
        .map(|user| {
          let verification = backend.contact_verification(user.id);
          (user, verification.is_verified)
        })
        .collect::<Vec<_>>()
    })
  });

  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("settings-devices-title")}</legend>
      <p class="muted-text">{move || i18n.t("settings-devices-hint")}</p>
      <ul class="device-list">
        <For each=move || devices.get() key=|device| device.id let:device>
          {
            let id = device.id;
            let is_pending = move || pending.get() == Some(id);
            let seen = move || format_relative(device.last_seen, now, i18n.locale().tag());
            view! {
              <li class="device">
                <Icon name=IconName::Smartphone />
                <div class="device-info">
                  <span translate="no">{device.name.clone()}</span>
                  <span class="muted-text small">
                    {move || if device.is_current { i18n.t("settings-device-current") } else { seen() }}
                  </span>
                </div>
                <Show when=move || !device.is_current fallback=|| ()>
                  <Show
                    when=is_pending
                    fallback=move || {
                      view! {
                        <button class="btn btn-small" on:click=move |_| pending.set(Some(id))>
                          {move || i18n.t("settings-device-sign-out")}
                        </button>
                      }
                    }
                  >
                    <button
                      class="btn btn-small btn-danger"
                      on:click=move |_| {
                        state.write(|backend| backend.remove_my_device(id));
                        pending.set(None);
                      }
                    >
                      {move || i18n.t("settings-device-sign-out-confirm")}
                    </button>
                    <button class="btn btn-small" on:click=move |_| pending.set(None)>{move || i18n.t("cancel")}</button>
                  </Show>
                </Show>
              </li>
            }
          }
        </For>
        <Show when=move || devices.with(Vec::is_empty) fallback=|| ()>
          <li class="muted-text">{move || i18n.t("settings-devices-unavailable")}</li>
        </Show>
      </ul>
      <div class="detail-actions">
        <button class="btn" on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
          <Icon name=IconName::QrCode />
          <span>{move || i18n.t("settings-device-link")}</span>
        </button>
      </div>
    </fieldset>

    <fieldset class="settings-group">
      <legend>{move || i18n.t("security-my-key")}</legend>
      <p class="muted-text small">{move || i18n.t("security-my-key-hint")}</p>
      <Show
        when=move || !fingerprint.with(String::is_empty)
        fallback=move || view! { <p class="muted-text small">{move || i18n.t("security-my-key-unavailable")}</p> }
      >
        <p class="fingerprint" translate="no">{move || fingerprint.get()}</p>
      </Show>
    </fieldset>

    <Show when=|| crate::auth::tauri::has_core() fallback=|| ()>
      <RecoveryPassphrase />
    </Show>

    <fieldset class="settings-group">
      <legend>{move || i18n.t("security-verify")}</legend>
      <p class="muted-text small">{move || i18n.t("security-verify-hint")}</p>
      <ul class="device-list">
        <For each=move || contacts.get() key=|(user, verified)| (user.id, *verified) let:entry>
          {
            let (user, is_verified) = entry;
            let user_id: UserId = user.id;
            view! {
              <li class="device">
                <Icon name=IconName::User />
                <div class="device-info">
                  <span translate="no">{user.display_name.clone()}</span>
                  <span class="small" class:muted-text=!is_verified class:ok-text=is_verified>
                    {move || if is_verified { i18n.t("contact-verified-short") } else { i18n.t("contact-not-verified") }}
                  </span>
                </div>
                <button class="btn btn-small" on:click=move |_| state.overlay.set(Overlay::VerifyContact(user_id))>
                  {move || i18n.t("security-compare")}
                </button>
              </li>
            }
          }
        </For>
      </ul>
    </fieldset>
  }
}

#[component]
pub fn CallsTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let calls = Memo::new(move |_| prefs.get().calls);
  let devices = state.read(|backend| backend.media_devices());
  let device_options = |list: &[crate::calls::MediaDevice]| -> Vec<(String, String)> {
    list
      .iter()
      .map(|device| (device.id.clone(), device.label.clone()))
      .collect()
  };
  let effect = RwSignal::new(effect_from_slug(&calls.get_untracked().background));
  // Picker changes flow into the preferences.
  Effect::new(move |previous: Option<()>| {
    let slug = effect_slug(effect.get());
    if previous.is_some() {
      prefs.update(|all| all.calls.background = slug);
    }
  });

  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("calls-devices")}</legend>
      <p class="muted-text small">{move || i18n.t("calls-devices-hint")}</p>
      <DeviceField
        id="calls-camera"
        label="prejoin-camera"
        list=device_options(&devices.cameras)
        value=Signal::derive(move || calls.get().camera_id)
        on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.camera_id = id))
      />
      <DeviceField
        id="calls-mic"
        label="prejoin-microphone"
        list=device_options(&devices.microphones)
        value=Signal::derive(move || calls.get().microphone_id)
        on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.microphone_id = id))
      />
      <DeviceField
        id="calls-speaker"
        label="prejoin-speaker"
        list=device_options(&devices.speakers)
        value=Signal::derive(move || calls.get().speaker_id)
        on_change=Callback::new(move |id: String| prefs.update(|all| all.calls.speaker_id = id))
      />
    </fieldset>
    <fieldset class="settings-group">
      <legend>{move || i18n.t("calls-joining")}</legend>
      <CheckField
        label="calls-start-muted"
        value=Signal::derive(move || calls.get().start_muted)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.start_muted = value))
      />
      <CheckField
        label="calls-start-camera-off"
        value=Signal::derive(move || calls.get().start_camera_off)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.start_camera_off = value))
      />
      <CheckField
        label="prejoin-mirror"
        value=Signal::derive(move || calls.get().mirror_self_view)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.mirror_self_view = value))
      />
    </fieldset>
    <fieldset class="settings-group">
      <legend>{move || i18n.t("calls-audio")}</legend>
      <CheckField
        label="calls-noise"
        value=Signal::derive(move || calls.get().noise_suppression)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.noise_suppression = value))
      />
      <CheckField
        label="calls-echo"
        value=Signal::derive(move || calls.get().echo_cancellation)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.echo_cancellation = value))
      />
      <CheckField
        label="calls-ptt"
        hint="calls-ptt-hint"
        value=Signal::derive(move || calls.get().push_to_talk)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.push_to_talk = value))
      />
    </fieldset>
    <fieldset class="settings-group">
      <legend>{move || i18n.t("calls-background")}</legend>
      <BackgroundPicker effect=effect />
      <p class="muted-text small">{move || i18n.t("calls-background-hint")}</p>
    </fieldset>
  }
}

/// Device choice; the empty value means "system default".
#[component]
fn DeviceField(
  id: &'static str,
  label: &'static str,
  list: Vec<(String, String)>,
  #[prop(into)] value: Signal<String>,
  on_change: Callback<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <select id=id class="input" on:change=move |ev| on_change.run(event_target_value(&ev))>
        <option value="" selected=move || value.get().is_empty()>
          {move || i18n.t("calls-system-default")}
        </option>
        {list
          .into_iter()
          .map(|(device_id, name)| {
            let check = device_id.clone();
            view! {
              <option value=device_id selected=move || value.get() == check>
                {name}
              </option>
            }
          })
          .collect_view()}
      </select>
    </div>
  }
}

/// Applies call preferences to the running call session (start state, effect, mirror, devices).
pub fn apply_call_prefs(call: crate::calls::CallState, calls: &CallPrefs) {
  call.is_mic_on.set(!calls.start_muted);
  call.is_camera_on.set(!calls.start_camera_off);
  call.mirror_self_view.set(calls.mirror_self_view);
  call.effect.set(effect_from_slug(&calls.background));
  call.camera_id.set(calls.camera_id.clone());
  call.microphone_id.set(calls.microphone_id.clone());
  call.speaker_id.set(calls.speaker_id.clone());
  call.share_sound.set(calls.share_sound);
}

/// Replaces the recovery passphrase: the key backup is wrapped with the new one (e2ee.md §6.2).
#[component]
fn RecoveryPassphrase() -> impl IntoView {
  /// Same rule as at enrollment: short passphrases are cheap to guess offline.
  const MIN_CHARS: usize = 12;
  let i18n = use_i18n();
  let passphrase = RwSignal::new(String::new());
  let confirm = RwSignal::new(String::new());
  let outcome = RwSignal::new(None::<Result<(), String>>);
  let is_working = RwSignal::new(false);
  let can_save = move || {
    !is_working.get() && passphrase.get().chars().count() >= MIN_CHARS && passphrase.get() == confirm.get()
  };
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if !can_save() {
      return;
    }
    is_working.set(true);
    let new_passphrase = passphrase.get_untracked();
    leptos::task::spawn_local(async move {
      let result = crate::remote_backend::change_passphrase(&new_passphrase).await;
      if result.is_ok() {
        passphrase.set(String::new());
        confirm.set(String::new());
      }
      outcome.set(Some(result));
      is_working.set(false);
    });
  };
  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("recovery-title")}</legend>
      <p class="muted-text small">{move || i18n.t("recovery-hint")}</p>
      <form class="auth-form" on:submit=save>
        <label for="recovery-new">{move || i18n.t("recovery-new")}</label>
        <input
          id="recovery-new"
          class="input"
          type="password"
          autocomplete="new-password"
          prop:value=move || passphrase.get()
          on:input=move |ev| passphrase.set(event_target_value(&ev))
        />
        <label for="recovery-confirm">{move || i18n.t("enroll-confirm")}</label>
        <input
          id="recovery-confirm"
          class="input"
          type="password"
          autocomplete="new-password"
          prop:value=move || confirm.get()
          on:input=move |ev| confirm.set(event_target_value(&ev))
        />
        <p class="muted-text small">{move || i18n.t_with("enroll-new-hint", &[("count", &MIN_CHARS.to_string())])}</p>
        {move || {
          outcome.get().map(|result| match result {
            Ok(()) => view! { <p class="ok-text" role="status">{move || i18n.t("recovery-done")}</p> }.into_any(),
            Err(message) => {
              view! {
                <p class="field-error" role="alert">{move || i18n.t("recovery-failed")}</p>
                <p class="small mono" translate="no">{message}</p>
              }
              .into_any()
            },
          })
        }}
        <div class="auth-actions">
          <button type="submit" class="btn btn-primary" prop:disabled=move || !can_save()>
            {move || i18n.t("recovery-save")}
          </button>
        </div>
      </form>
    </fieldset>
  }
}
