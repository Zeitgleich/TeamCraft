use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_proto::Role;

use crate::admin::AdminSection;
use crate::clock::{
  end_of_local_day, end_of_local_week, format_date_time, format_local_datetime_input, format_time, local_day,
  now_ms, parse_local_datetime,
};
use crate::components::avatar::{Avatar, AvatarSize, PresenceDot};
use crate::components::emoji_picker::EmojiPicker;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::profile_views::status_until_label;
use crate::components::sheet::MenuSurface;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::model::{CustomStatus, PresenceStatus, StatusExpiry, TimestampMs};
use crate::session::use_session;
use crate::state::{AppState, Overlay, SettingsTab, use_app_state};

/// "Until tomorrow morning" (the bell and the tray) means 08:00 local time.
pub(crate) const MORNING_MINUTE: u16 = 8 * 60;
pub(crate) const MS_PER_HOUR: TimestampMs = 3_600_000;
/// The shortest pause of notifications and the shortest "Reset after": focus button, tray and the
/// status's duration offer it.
pub(crate) const FOCUS_SHORT_MS: TimestampMs = 30 * 60_000;

/// Suggestions while the workspace configured no presets of its own; the label is an i18n key.
const STATUS_PRESETS: [(&str, &str); 4] = [
  ("🗓️", "status-preset-meeting"),
  ("🚌", "status-preset-commuting"),
  ("🤒", "status-preset-sick"),
  ("🌴", "status-preset-vacation"),
];

/// Popover under the avatar: the status list (#1578), custom status and account shortcuts.
#[component]
pub fn StatusMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let menu = NodeRef::<html::Div>::new();
  let session = use_session();
  let me = Memo::new(move |_| state.me());
  let is_admin = Memo::new(move |_| state.read(|backend| backend.my_role()).includes(&Role::Admin));
  // Alerts of the health checks, told to admins: the entry says so and opens the health page.
  let health_alerts = Memo::new(move |_| state.read(|backend| backend.health_alert_count()));
  // The workspace may switch statuses off (PLAN.md §4): then there is nothing to set.
  let are_statuses_enabled =
    Memo::new(move |_| state.read(|backend| backend.presence_policy()).statuses_enabled);

  Effect::new(move |_| {
    if let Some(element) = menu.get() {
      let target = element.query_selector("[role^='menuitem']").ok().flatten();
      if let Some(target) = target.and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok()) {
        drop(target.focus());
      }
    }
  });

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let key = ev.key();
    match key.as_str() {
      "Escape" => {
        ev.stop_propagation();
        state.close_overlay();
        focus_account_button();
      },
      "ArrowDown" | "ArrowUp" | "Home" | "End" => {
        ev.prevent_default();
        move_focus(&ev, menu, &key);
      },
      "Tab" => state.close_overlay(),
      _ => {},
    }
  };

  let set_presence = move |presence: PresenceStatus| {
    state.set_presence_until(presence, None);
    state.close_overlay();
    focus_account_button();
  };
  // A status the user set themselves can be reset; available is automatic, and busy during a call
  // on this device is the server's doing.
  let is_manual = Memo::new(move |_| match me.get().presence {
    PresenceStatus::Online => false,
    PresenceStatus::Busy => state.call.phase.get() == teamcraft_client_api::calls::CallPhase::Idle,
    _ => true,
  });
  let status_item = move |presence: PresenceStatus| {
    let is_current = move || me.get().presence == presence;
    view! {
      <button
        class="menu-item"
        role="menuitemradio"
        tabindex="-1"
        aria-checked=move || is_current().to_string()
        on:click=move |_| set_presence(presence)
      >
        <PresenceDot presence=presence />
        <span class="menu-label">{move || i18n.t(pick_key(presence))}</span>
        {move || is_current().then(|| view! { <Icon name=IconName::Check /> })}
      </button>
    }
  };

  view! {
    <MenuSurface
      class="status-menu"
      sheet_class="status-menu"
      label=Signal::derive(move || i18n.t("status-menu-label"))
      on_close=Callback::new(move |()| state.close_overlay())
      node_ref=menu
      on_keydown=Callback::new(on_keydown)
    >
      <div class="menu-user">
        {move || view! { <Avatar user=me.get() size=AvatarSize::Large /> }}
        <div>
          <div class="menu-user-name" translate="no">
            {move || me.get().display_name}
          </div>
          <div class="menu-user-sub menu-user-status">
            {move || view! { <PresenceDot presence=me.get().presence /> }}
            <span>
              {move || i18n.t(&format!("presence-{}", me.get().presence.slug()))}
              {move || status_end_label(state, i18n).map(|until| format!(" · {until}"))}
            </span>
          </div>
          <div class="menu-user-sub" translate="no">
            {move || format!("@{}", me.get().username)}
          </div>
        </div>
      </div>
      <Show when=move || are_statuses_enabled.get() fallback=|| ()>
        <button
          class="menu-item custom-status-row"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| state.overlay.set(Overlay::CustomStatus)
        >
          // An empty status (no emoji, no text) is no status: offer to set one.
          {move || match me.get().maybe_custom_status.filter(|status| !status.text.trim().is_empty() || !status.emoji.trim().is_empty()) {
            Some(status) => {
              let maybe_until = status.maybe_expires_at.map(|at| status_until_label(at, now_ms(), i18n));
              view! {
                <span class="menu-emoji" aria-hidden="true">{status.emoji}</span>
                <span class="menu-label">
                  {status.text}
                  {maybe_until.map(|until| view! { <small>{until}</small> })}
                </span>
              }
              .into_any()
            },
            None => {
              view! {
                <Icon name=IconName::Smile />
                <span class="menu-label muted-text">{move || i18n.t("status-custom-set")}</span>
              }
              .into_any()
            },
          }}
        </button>
        <div class="menu-sep" role="separator"></div>
      </Show>
      // The Teams-style status list (#1578), then profile, preferences, sign out (#299).
      <div class="menu-sub" role="group" aria-label=move || i18n.t("status-pick-label")>
        {PresenceStatus::ALL.map(status_item).collect_view()}
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| state.overlay.set(Overlay::StatusDuration)
        >
          <Icon name=IconName::Clock />
          <span class="menu-label">{move || i18n.t("status-duration")}</span>
        </button>
        <Show when=move || is_manual.get() fallback=|| ()>
          <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| set_presence(PresenceStatus::Online)>
            <Icon name=IconName::Undo />
            <span class="menu-label">{move || i18n.t("status-reset")}</span>
          </button>
        </Show>
      </div>
      <div class="menu-sep" role="separator"></div>
      <Show when=|| crate::profiles::use_profiles().is_some() fallback=|| ()>
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.open_settings_at(SettingsTab::Account)>
          <Icon name=IconName::User />
          <span class="menu-label">{move || i18n.t("profile-edit-title")}</span>
        </button>
      </Show>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.open_settings()>
        <Icon name=IconName::Sliders />
        <span class="menu-label">{move || i18n.t("menu-settings")}</span>
      </button>
      <Show when=move || is_admin.get() fallback=|| ()>
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| {
            state
              .open_admin(
                if health_alerts.get_untracked() > 0 { AdminSection::Health } else { AdminSection::Channels },
              )
          }
        >
          <Icon name=IconName::Shield />
          <span class="menu-label">{move || i18n.t("menu-administration")}</span>
          <Show when=move || { health_alerts.get() > 0 } fallback=|| ()>
            <span class="badge-count" role="img" aria-label=move || i18n.t_count("health-alerts-badge", health_alerts.get())>
              {move || health_alerts.get()}
            </span>
          </Show>
        </button>
      </Show>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::Shortcuts)>
        <Icon name=IconName::Keyboard />
        <span class="menu-label">{move || i18n.t("shortcuts-title")}</span>
      </button>
      // Every member can report; guests share no DM with the organization's admins.
      <Show when=move || !state.me().is_guest fallback=|| ()>
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::ReportProblem)>
          <Icon name=IconName::Flag />
          <span class="menu-label">{move || i18n.t("report-menu")}</span>
        </button>
      </Show>
      <div class="menu-sep" role="separator"></div>
      <button class="menu-item" role="menuitem" tabindex="-1" disabled title=move || i18n.t("admin-soon")>
        <Icon name=IconName::Users />
        <span class="menu-label">{move || i18n.t("menu-switch-account")}</span>
      </button>
      <button
        class="menu-item"
        role="menuitem"
        tabindex="-1"
        disabled=session.is_none()
        title=move || if session.is_none() { i18n.t("menu-log-out-demo") } else { String::new() }
        on:click=move |_| {
          if let Some(control) = session {
            state.close_overlay();
            control.sign_out();
          }
        }
      >
        <Icon name=IconName::Close />
        <span class="menu-label">{move || i18n.t("menu-log-out")}</span>
      </button>
    </MenuSurface>
  }
}

/// "Until 14:30" (or a date and time when it ends on another day) while notifications are paused
/// with an end; nothing while they aren't, or when the pause has no end.
pub(crate) fn paused_until_label(state: AppState, i18n: I18n) -> Option<String> {
  if state.me().presence != PresenceStatus::DoNotDisturb {
    return None;
  }
  let until = state
    .settings
    .prefs
    .with(|prefs| prefs.notifications.maybe_dnd_until)?;
  until_label(until, i18n)
}

/// "Until 14:30" while the user's manual status has an end ("Reset after"); nothing for a status
/// without one, or once the shown status is another.
pub(crate) fn status_end_label(state: AppState, i18n: I18n) -> Option<String> {
  let presence = state.me().presence;
  let end = state
    .settings
    .prefs
    .with(|prefs| prefs.privacy.maybe_status_end)?;
  if end.status != presence {
    return paused_until_label(state, i18n);
  }
  until_label(end.until, i18n)
}

fn until_label(until: TimestampMs, i18n: I18n) -> Option<String> {
  let now = now_ms();
  if until <= now {
    return None;
  }
  let locale = i18n.locale().tag();
  let time = if local_day(until) == local_day(now) {
    format_time(until, locale)
  } else {
    format_date_time(until, locale)
  };
  Some(i18n.t_with("presence-dnd-until", &[("time", &time)]))
}

/// The status list's wording: "Available", "Appear away", "Appear offline" say what the user does,
/// where others see "Online", "Away", "Offline".
pub(crate) fn pick_key(presence: PresenceStatus) -> &'static str {
  match presence {
    PresenceStatus::Online => "status-pick-online",
    PresenceStatus::Busy => "status-pick-busy",
    PresenceStatus::DoNotDisturb => "status-pick-dnd",
    PresenceStatus::BeRightBack => "status-pick-brb",
    PresenceStatus::Away => "status-pick-away",
    PresenceStatus::Offline => "status-pick-offline",
  }
}

/// "Reset after" of a manual status (#1578), as Teams' "Duration".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetAfter {
  ThirtyMinutes,
  OneHour,
  TwoHours,
  Today,
  ThisWeek,
  /// A time the user enters.
  UntilTime,
  Never,
}

impl ResetAfter {
  pub(crate) const ALL: [Self; 7] = [
    Self::ThirtyMinutes,
    Self::OneHour,
    Self::TwoHours,
    Self::Today,
    Self::ThisWeek,
    Self::UntilTime,
    Self::Never,
  ];

  pub(crate) fn slug(self) -> &'static str {
    match self {
      Self::ThirtyMinutes => "30m",
      Self::OneHour => "1h",
      Self::TwoHours => "2h",
      Self::Today => "today",
      Self::ThisWeek => "week",
      Self::UntilTime => "until",
      Self::Never => "never",
    }
  }

  pub(crate) fn from_slug(slug: &str) -> Self {
    Self::ALL
      .into_iter()
      .find(|choice| choice.slug() == slug)
      .unwrap_or(Self::Today)
  }

  /// When a status set at `now` ends; `None` for never, and for a time the user enters.
  pub(crate) fn end(self, now: TimestampMs) -> Option<TimestampMs> {
    match self {
      Self::ThirtyMinutes => Some(now + FOCUS_SHORT_MS),
      Self::OneHour => Some(now + MS_PER_HOUR),
      Self::TwoHours => Some(now + 2 * MS_PER_HOUR),
      Self::Today => Some(end_of_local_day(now)),
      Self::ThisWeek => Some(end_of_local_week(now)),
      Self::UntilTime | Self::Never => None,
    }
  }
}

/// The statuses a duration applies to: every manual one (available is automatic).
const MANUAL_STATUSES: [PresenceStatus; 5] = [
  PresenceStatus::Busy,
  PresenceStatus::DoNotDisturb,
  PresenceStatus::BeRightBack,
  PresenceStatus::Away,
  PresenceStatus::Offline,
];
const DURATION_FORM_ID: &str = "status-duration-form";
const UNTIL_FIELD: &str = "status-duration-until";

/// "Duration…": a manual status with a "Reset after" (#1578). Afterwards the server makes presence
/// automatic again.
#[component]
pub fn StatusDurationDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = state.me().presence;
  let status = RwSignal::new(if MANUAL_STATUSES.contains(&current) {
    current
  } else {
    PresenceStatus::Busy
  });
  let reset_after = RwSignal::new(ResetAfter::OneHour);
  let custom_until = RwSignal::new(format_local_datetime_input(now_ms() + MS_PER_HOUR));
  let errors = FormErrors::new();
  let check = move || -> Result<Option<TimestampMs>, FieldErrors> {
    let now = now_ms();
    match reset_after.get_untracked() {
      ResetAfter::UntilTime => {
        match parse_local_datetime(&custom_until.get_untracked()).filter(|until| *until > now) {
          Some(until) => Ok(Some(until)),
          None => {
            let mut found = FieldErrors::new();
            found.push(UNTIL_FIELD, i18n.t("status-duration-until-past"));
            Err(found)
          },
        }
      },
      other => Ok(other.end(now)),
    }
  };
  let save = move || match check() {
    Ok(maybe_until) => {
      state.set_presence_until(status.get_untracked(), maybe_until);
      state.close_overlay();
    },
    Err(found) => errors.refuse(DURATION_FORM_ID, found),
  };

  view! {
    <Modal labelled_by="status-duration-title" class="modal-small">
      <form
        id=DURATION_FORM_ID
        novalidate
        on:submit=move |ev| {
          ev.prevent_default();
          save();
        }
      >
        <header class="modal-head">
          <h2 id="status-duration-title">{move || i18n.t("status-duration-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <div class="field">
            <label for="status-duration-status">{move || i18n.t("status-pick-label")}</label>
            <select
              id="status-duration-status"
              class="input"
              data-autofocus
              on:change=move |ev| {
                let slug = event_target_value(&ev);
                if let Some(picked) = MANUAL_STATUSES.into_iter().find(|presence| presence.slug() == slug) {
                  status.set(picked);
                }
              }
            >
              {MANUAL_STATUSES
                .map(|presence| {
                  view! {
                    <option value=presence.slug() selected=move || status.get() == presence>
                      {move || i18n.t(pick_key(presence))}
                    </option>
                  }
                })
                .collect_view()}
            </select>
            <Show when=move || status.get() == PresenceStatus::DoNotDisturb fallback=|| ()>
              <p class="hint">{move || i18n.t("status-duration-dnd-hint")}</p>
            </Show>
          </div>
          <div class="field">
            <label for="status-duration-reset">{move || i18n.t("status-duration-reset-after")}</label>
            <select
              id="status-duration-reset"
              class="input"
              on:change=move |ev| {
                reset_after.set(ResetAfter::from_slug(&event_target_value(&ev)));
                errors.follow(check().err().unwrap_or_default());
              }
            >
              {ResetAfter::ALL
                .map(|choice| {
                  view! {
                    <option value=choice.slug() selected=move || reset_after.get() == choice>
                      {move || i18n.t(&format!("status-duration-{}", choice.slug()))}
                    </option>
                  }
                })
                .collect_view()}
            </select>
          </div>
          // Disabled rather than hidden would leave an empty field for most choices; it only
          // appears below the select, so nothing above it moves.
          <Show when=move || reset_after.get() == ResetAfter::UntilTime fallback=|| ()>
            <div class="field">
              <label for=UNTIL_FIELD>{move || i18n.t("status-duration-until-label")}</label>
              <input
                id=UNTIL_FIELD
                class="input"
                type="datetime-local"
                aria-invalid=errors.invalid(UNTIL_FIELD)
                aria-describedby=errors.described(UNTIL_FIELD)
                prop:value=move || custom_until.get()
                on:input=move |ev| {
                  custom_until.set(event_target_value(&ev));
                  errors.follow(check().err().unwrap_or_default());
                }
              />
              <FieldError errors=errors field=UNTIL_FIELD />
            </div>
          </Show>
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>
            {move || i18n.t("cancel")}
          </button>
          <button type="submit" class="btn btn-primary">
            {move || i18n.t("status-duration-set")}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// Dialog to set emoji, text and expiry of the custom status. Offers the workspace's presets (or
/// suggestions when it has none); with custom statuses switched off only the presets can be picked.
#[component]
pub fn CustomStatusDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = state.read(|backend| backend.presence_policy());
  let can_write_own = policy.custom_status_enabled;
  let current = state.me().maybe_custom_status;
  // The end of the current status, or of a preset with a duration; picking a "clear after"
  // choice replaces it.
  let chosen_end = RwSignal::new(current.as_ref().and_then(|status| status.maybe_expires_at));
  let emoji = RwSignal::new(
    current
      .as_ref()
      .map_or_else(String::new, |status| status.emoji.clone()),
  );
  let text = RwSignal::new(
    current
      .as_ref()
      .map(|status| status.text.clone())
      .unwrap_or_default(),
  );
  let expiry = RwSignal::new(
    current
      .as_ref()
      .map_or(StatusExpiry::Today, |status| status.expiry),
  );
  let has_current = current.is_some();
  let presets: Vec<Preset> = if policy.presets.is_empty() {
    if can_write_own {
      STATUS_PRESETS
        .into_iter()
        .map(|(emoji, key)| Preset {
          emoji: emoji.to_owned(),
          text: i18n.t(key),
          maybe_duration_minutes: None,
        })
        .collect()
    } else {
      Vec::new()
    }
  } else {
    policy
      .presets
      .iter()
      .map(|preset| Preset {
        emoji: preset.maybe_emoji.clone().unwrap_or_default(),
        text: preset.text.clone(),
        maybe_duration_minutes: preset.maybe_duration_minutes,
      })
      .collect()
  };
  let has_presets = !presets.is_empty();
  let text_input = NodeRef::<html::Input>::new();

  let save = move || {
    let status = CustomStatus {
      emoji: emoji.get_untracked().trim().to_owned(),
      text: text.get_untracked().trim().to_owned(),
      expiry: expiry.get_untracked(),
      maybe_expires_at: chosen_end.get_untracked().filter(|end| *end > now_ms()),
    };
    // Without text there is no status (the emoji alone is the default one).
    let is_empty = status.text.trim().is_empty();
    state.write(|backend| backend.set_custom_status((!is_empty).then_some(status)));
    state.close_overlay();
  };
  let chosen_end_label = move || {
    chosen_end
      .get()
      .map(|end| status_until_label(end, now_ms(), i18n))
  };

  view! {
    <Modal labelled_by="custom-status-title" class="modal-small">
      <form
        on:submit=move |ev| {
          ev.prevent_default();
          save();
        }
      >
        <header class="modal-head">
          <h2 id="custom-status-title">{move || i18n.t("status-custom-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <Show when=move || !can_write_own fallback=|| ()>
            <p class="hint">{move || i18n.t("status-presets-only")}</p>
          </Show>
          <Show when=move || can_write_own || !has_presets fallback=|| ()>
          <div class="field-row">
            <StatusEmojiButton emoji=emoji is_enabled=can_write_own text_input=text_input />
            <label class="visually-hidden" for="status-text">{move || i18n.t("status-text")}</label>
            <input
              id="status-text"
              class="input grow"
              node_ref=text_input
              name="status"
              data-autofocus
              autocomplete="off"
              maxlength="100"
              placeholder=move || i18n.t("status-text-placeholder")
              readonly=!can_write_own
              prop:value=move || text.get()
              on:input=move |ev| text.set(event_target_value(&ev))
            />
          </div>
          </Show>
          <ul class="preset-list" aria-label=move || i18n.t("status-presets")>
            {presets
              .into_iter()
              .map(|preset| {
                let Preset { emoji: preset_emoji, text: preset_text, maybe_duration_minutes } = preset;
                let shown_emoji = preset_emoji.clone();
                let shown_text = preset_text.clone();
                let is_picked = {
                  let (preset_emoji, preset_text) = (preset_emoji.clone(), preset_text.clone());
                  move || emoji.get() == preset_emoji && text.get() == preset_text
                };
                view! {
                  <li>
                    <button
                      type="button"
                      class="preset"
                      aria-pressed=move || is_picked().to_string()
                      on:click=move |_| {
                        emoji.set(preset_emoji.clone());
                        text.set(preset_text.clone());
                        // A preset's duration is its end, unless the user picks another below.
                        if let Some(minutes) = maybe_duration_minutes {
                          chosen_end.set(Some(now_ms() + i64::from(minutes) * 60_000));
                        }
                      }
                    >
                      <span aria-hidden="true">{shown_emoji}</span>
                      <span translate="no">{shown_text}</span>
                    </button>
                  </li>
                }
              })
              .collect_view()}
          </ul>
          <div class="field">
            <label for="status-expiry">{move || i18n.t("status-clear-after")}</label>
            <select
              id="status-expiry"
              class="input"
              on:change=move |ev| {
                expiry.set(StatusExpiry::from_slug(&event_target_value(&ev)));
                chosen_end.set(None);
              }
            >
              {StatusExpiry::ALL
                .into_iter()
                .map(|option| {
                  view! {
                    <option value=option.slug() selected=move || expiry.get() == option>
                      {move || i18n.t(&format!("status-expiry-{}", option.slug()))}
                    </option>
                  }
                })
                .collect_view()}
            </select>
            {move || chosen_end_label().map(|label| view! { <p class="hint">{label}</p> })}
          </div>
        </div>
        <footer class="modal-foot">
          {has_current.then(|| {
            view! {
              <button
                type="button"
                class="btn btn-quiet"
                on:click=move |_| {
                  state.write(|backend| backend.set_custom_status(None));
                  state.close_overlay();
                }
              >
                {move || i18n.t("status-clear")}
              </button>
            }
          })}
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>
            {move || i18n.t("cancel")}
          </button>
          <button type="submit" class="btn btn-primary">
            {move || i18n.t("save")}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// The status emoji as a compact square button (#705): it shows the chosen emoji, or a smiley while
/// there is none, and opens the app's emoji picker. Picking closes it and moves on to the text;
/// Backspace or Delete on the button removes the emoji.
#[component]
fn StatusEmojiButton(
  emoji: RwSignal<String>,
  is_enabled: bool,
  text_input: NodeRef<html::Input>,
) -> impl IntoView {
  let i18n = use_i18n();
  let button = NodeRef::<html::Button>::new();
  let maybe_anchor = RwSignal::new_local(None::<web_sys::Element>);
  let on_pick = move |picked: String| {
    emoji.set(picked);
    maybe_anchor.set(None);
    if let Some(element) = text_input.get_untracked() {
      drop(element.focus());
    }
  };
  let on_close = move |()| {
    maybe_anchor.set(None);
    if let Some(element) = button.get_untracked() {
      drop(element.focus());
    }
  };
  view! {
    <button
      type="button"
      id="status-emoji"
      class="emoji-pick-btn"
      node_ref=button
      aria-label=move || i18n.t("status-emoji")
      title=move || i18n.t("status-emoji")
      aria-haspopup="dialog"
      aria-expanded=move || maybe_anchor.with(Option::is_some).to_string()
      disabled=!is_enabled
      on:click=move |ev| maybe_anchor.set(Some(event_target::<web_sys::Element>(&ev)))
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if matches!(ev.key().as_str(), "Backspace" | "Delete") {
          ev.prevent_default();
          emoji.set(String::new());
        }
      }
    >
      {move || {
        let chosen = emoji.get();
        if chosen.trim().is_empty() {
          view! { <Icon name=IconName::Smile /> }.into_any()
        } else {
          view! { <span class="emoji-pick-value" aria-hidden="true">{chosen}</span> }.into_any()
        }
      }}
    </button>
    {move || {
      maybe_anchor.get().map(|anchor| {
        view! {
          <EmojiPicker anchor=anchor returns_focus=false on_pick=Callback::new(on_pick) on_close=Callback::new(on_close) />
        }
      })
    }}
  }
}

/// A status the dialog offers to pick.
struct Preset {
  emoji: String,
  text: String,
  maybe_duration_minutes: Option<u32>,
}

fn move_focus(ev: &web_sys::KeyboardEvent, menu: NodeRef<html::Div>, key: &str) {
  let Some(root) = menu.get_untracked() else { return };
  let Ok(list) = root.query_selector_all("[role^='menuitem']:not([disabled])") else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|i| list.item(i))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  if items.is_empty() {
    return;
  }
  let active = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok());
  let position = active.and_then(|active| items.iter().position(|item| item.is_same_node(Some(&active))));
  let target = match (key, position) {
    ("Home", _) => 0,
    ("End", _) => items.len() - 1,
    ("ArrowUp", Some(position)) => (position + items.len() - 1) % items.len(),
    ("ArrowUp", None) => items.len() - 1,
    (_, Some(position)) => (position + 1) % items.len(),
    (_, None) => 0,
  };
  drop(items[target].focus());
}

fn focus_account_button() {
  let button = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(".avatar-btn").ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(button) = button {
    drop(button.focus());
  }
}
