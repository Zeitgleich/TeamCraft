//! Profiles: the card of another person with their local time, and the helpers the profile form
//! (Settings > Account, `settings_account.rs`) shares with it.

use leptos::prelude::*;
use teamcraft_proto::{UserId, WorkingHours};

use crate::clock::{format_date_time, format_relative, now_ms, time_in_zone};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{PresenceStatus, User};
use crate::profiles::use_profiles;
use crate::state::use_app_state;

pub use teamcraft_proto::Weekday;

/// Below this, "last seen" reads "just now": seconds are noise, and the label doesn't tick.
const LAST_SEEN_JUST_NOW_MS: i64 = 2 * 60_000;

/// `HH:MM` (what `<input type=time>` holds) to minutes since midnight.
pub fn parse_minutes(text: &str) -> Option<u16> {
  let (hours, minutes) = text.split_once(':')?;
  let (hours, minutes): (u16, u16) = (hours.parse().ok()?, minutes.parse().ok()?);
  (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

pub fn format_minutes(minutes: u16) -> String {
  format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// "In a meeting" next to a person who is in a call right now (from the server's call rosters).
#[component]
pub fn InMeetingBadge(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Show when=move || state.read(|backend| backend.is_in_call(user)) fallback=|| ()>
      <span class="in-meeting-badge" title=move || i18n.t("presence-in-meeting")>
        <Icon name=IconName::Phone />
        <span class="visually-hidden">{move || i18n.t("presence-in-meeting")}</span>
      </span>
    </Show>
  }
}

/// "Last seen 5 minutes ago" for a person who is offline and lets others see it. Shows nothing
/// while they are online, hide it, appear offline or the workspace switched last seen off.
#[component]
pub fn LastSeen(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  view! {
    {move || {
      let is_offline = state
        .read(|backend| backend.user(user))
        .is_some_and(|person| person.presence == PresenceStatus::Offline);
      let maybe_at = is_offline.then(|| state.read(|backend| backend.last_seen(user))).flatten();
      maybe_at.map(|at| {
        let now = now_ms();
        let label = if now - at < LAST_SEEN_JUST_NOW_MS {
          i18n.t("last-seen-just-now")
        } else {
          i18n.t_with("last-seen", &[("when", &format_relative(at, now, locale))])
        };
        let exact = format_date_time(at, locale);
        view! { <span class="last-seen" title=exact>{label}</span> }
      })
    }}
  }
}

/// Another person's card: name, status, role in the company, local time and a way to message them.
#[component]
pub fn UserProfileDialog(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let person: Option<User> = state.read(|backend| backend.user(user));
  let Some(person) = person else {
    return view! { <Modal labelled_by="profile-card-title"><p>{move || i18n.t("profile-unavailable")}</p></Modal> }.into_any();
  };
  let profile = move || use_profiles().and_then(|hub| hub.profile(user));
  let is_me = person.id == state.me().id;
  let name = person.display_name.clone();
  view! {
    <Modal labelled_by="profile-card-title" class="modal-small">
      <header class="modal-head">
        <h2 id="profile-card-title" translate="no">{name}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body profile-card">
        <Avatar user=person.clone() size=AvatarSize::Large show_presence=true />
        {person.maybe_custom_status.clone().map(|status| view! { <p>{status.emoji} " " {status.text}</p> })}
        <p><InMeetingBadge user=user /></p>
        <p><LastSeen user=user /></p>
        {person.maybe_organization.clone().map(|name| view! { <p><crate::components::org_badge::OrgBadge organization=Some(name) /></p> })}
        {move || {
          profile().map(|profile| {
            let zone = profile.maybe_time_zone.clone();
            let local = zone.as_deref().and_then(|zone| time_in_zone(now_ms(), zone, locale));
            view! {
              <dl class="profile-facts">
                {profile.maybe_title.map(|value| view! { <dt>{move || i18n.t("profile-title")}</dt><dd translate="no">{value}</dd> })}
                {profile.maybe_department.map(|value| view! { <dt>{move || i18n.t("profile-department")}</dt><dd translate="no">{value}</dd> })}
                {profile.maybe_phone.map(|value| view! { <dt>{move || i18n.t("profile-phone")}</dt><dd translate="no">{value}</dd> })}
                {local.map(|time| view! { <dt>{move || i18n.t("profile-local-time")}</dt><dd translate="no">{time} " · " {zone.clone().unwrap_or_default()}</dd> })}
                {profile.maybe_working_hours.map(|hours| view! {
                  <dt>{move || i18n.t("profile-hours")}</dt>
                  <dd>{working_hours_text(&hours, |key| i18n.t(key))}</dd>
                })}
              </dl>
            }
          })
        }}
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        {is_me
          .then(|| view! { <button class="btn" on:click=move |_| state.open_settings_at(crate::state::SettingsTab::Account)>{move || i18n.t("profile-edit-title")}</button> })}
        {(!is_me)
          .then(|| {
            view! {
              <button class="btn btn-primary" on:click=move |_| { state.close_overlay(); state.open_direct(user); }>
                {move || i18n.t("profile-message")}
              </button>
            }
          })}
      </footer>
    </Modal>
  }
  .into_any()
}

/// "Mon–Fri 09:00–17:00" style summary in the viewer's language.
pub fn working_hours_text(hours: &WorkingHours, translate: impl Fn(&str) -> String) -> String {
  let days: Vec<String> = crate::prefs::EVERY_DAY
    .iter()
    .filter(|day| hours.weekdays.contains(day))
    .map(|day| translate(&format!("weekday-{}", day.as_str())))
    .collect();
  format!(
    "{}, {}–{}",
    days.join(" "),
    format_minutes(hours.start_minute),
    format_minutes(hours.end_minute)
  )
}

/// The other person's local time for a direct conversation header, e.g. "14:32 Europe/Berlin".
pub fn local_time_label(user: UserId, locale: &'static str) -> Option<String> {
  let profile = use_profiles()?.profile(user)?;
  let zone = profile.maybe_time_zone?;
  let time = time_in_zone(now_ms(), &zone, locale)?;
  Some(format!("{time} · {zone}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn times_round_trip_and_reject_nonsense() {
    assert_eq!(parse_minutes("09:30"), Some(570));
    assert_eq!(format_minutes(570), "09:30");
    assert_eq!(parse_minutes("24:00"), None);
    assert_eq!(parse_minutes("9"), None);
  }

  #[test]
  fn working_hours_read_as_a_sentence() {
    let hours = WorkingHours {
      weekdays: vec![Weekday::Friday, Weekday::Monday],
      start_minute: 540,
      end_minute: 1020,
    };
    let text = working_hours_text(&hours, |key| key.trim_start_matches("weekday-").to_owned());
    assert_eq!(text, "mon fri, 09:00–17:00");
  }
}

/// The small card that appears when the pointer rests on a person's name: picture, status, local
/// time and a button for a direct message. Keyboard users open the full card with Enter instead.
#[component]
pub fn UserHoverCard(
  user: UserId,
  /// Fixed position (top, left) in pixels.
  top: f64,
  left: f64,
  on_enter: Callback<()>,
  on_leave: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let Some(person) = state.read(|backend| backend.user(user)) else {
    return ().into_any();
  };
  let is_me = person.id == state.me().id;
  let name = person.display_name.clone();
  let label = name.clone();
  let local_time = move || {
    let profile = use_profiles().and_then(|hub| hub.profile(user))?;
    let zone = profile.maybe_time_zone?;
    time_in_zone(now_ms(), &zone, locale)
  };
  view! {
    <div
      class="hover-card"
      role="dialog"
      aria-label=label
      style:top=format!("{top}px")
      style:left=format!("{left}px")
      on:mouseenter=move |_| on_enter.run(())
      on:mouseleave=move |_| on_leave.run(())
    >
      <div class="hover-card-head">
        <Avatar user=person.clone() size=AvatarSize::Large show_presence=true />
        <div>
          <strong translate="no">{name}</strong>
          {person.maybe_custom_status.clone().map(|status| view! { <p class="small">{status.emoji} " " {status.text}</p> })}
          {move || local_time().map(|time| view! { <p class="small muted-text">{move || i18n.t_with("hover-card-local-time", &[("time", &time)])}</p> })}
        </div>
      </div>
      <div class="hover-card-actions">
        {(!is_me).then(|| view! {
          <button class="btn btn-small btn-primary" on:click=move |_| { on_leave.run(()); state.open_direct(user); }>
            {move || i18n.t("profile-message")}
          </button>
        })}
        <button class="btn btn-small" on:click=move |_| { on_leave.run(()); state.overlay.set(crate::state::Overlay::UserProfile(user)); }>
          {move || i18n.t("hover-card-profile")}
        </button>
      </div>
    </div>
  }
  .into_any()
}
