//! Profiles: the form for the signed-in user (picture, title, department, phone, time zone, working
//! hours) and the card of another person with their local time.

use leptos::prelude::*;
use teamcraft_proto::{UpdateProfileRequest, UserId, WorkingHours};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::clock::{
  format_date_time, format_relative, local_time_zone, now_ms, time_in_zone, time_zone_names,
};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{PresenceStatus, User};
use crate::profiles::{MAX_AVATAR_BYTES, avatar_type, use_profiles};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

pub use teamcraft_proto::Weekday;

fn weekdays() -> [Weekday; 7] {
  [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
  ]
}
const DEFAULT_START: u16 = 9 * 60;
const DEFAULT_END: u16 = 17 * 60;
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

fn non_empty(text: &str) -> Option<String> {
  let trimmed = text.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The form of the signed-in user's own profile.
#[component]
pub fn EditProfileDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let Some(hub) = use_profiles() else {
    return view! { <Modal labelled_by="profile-title"><p>{move || i18n.t("profile-unavailable")}</p></Modal> }.into_any();
  };
  let me = state.me();
  let current = hub.profile(me.id);
  let title = RwSignal::new(String::new());
  let department = RwSignal::new(String::new());
  let phone = RwSignal::new(String::new());
  let time_zone = RwSignal::new(String::new());
  let has_hours = RwSignal::new(false);
  let days = RwSignal::new(vec![
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
  ]);
  let start = RwSignal::new(format_minutes(DEFAULT_START));
  let end = RwSignal::new(format_minutes(DEFAULT_END));
  let error = RwSignal::new(None::<String>);
  let is_saving = RwSignal::new(false);
  let zones = time_zone_names();

  // The stored profile arrives after the dialog opened: fill the form once.
  let is_filled = RwSignal::new(false);
  Effect::new(move |_| {
    if is_filled.get_untracked() {
      return;
    }
    let Some(profile) = hub.profile(me.id) else { return };
    is_filled.set(true);
    title.set(profile.maybe_title.unwrap_or_default());
    department.set(profile.maybe_department.unwrap_or_default());
    phone.set(profile.maybe_phone.unwrap_or_default());
    time_zone.set(
      profile
        .maybe_time_zone
        .unwrap_or_else(|| local_time_zone().unwrap_or_default()),
    );
    if let Some(hours) = profile.maybe_working_hours {
      has_hours.set(true);
      days.set(hours.weekdays);
      start.set(format_minutes(hours.start_minute));
      end.set(format_minutes(hours.end_minute));
    }
  });
  drop(current);

  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let maybe_working_hours = if has_hours.get_untracked() {
      let (Some(start_minute), Some(end_minute)) = (
        parse_minutes(&start.get_untracked()),
        parse_minutes(&end.get_untracked()),
      ) else {
        error.set(Some(i18n.t("profile-hours-invalid")));
        return;
      };
      if end_minute <= start_minute || days.with_untracked(Vec::is_empty) {
        error.set(Some(i18n.t("profile-hours-invalid")));
        return;
      }
      Some(WorkingHours {
        weekdays: days.get_untracked(),
        start_minute,
        end_minute,
      })
    } else {
      None
    };
    let request = UpdateProfileRequest {
      maybe_title: non_empty(&title.get_untracked()),
      maybe_department: non_empty(&department.get_untracked()),
      maybe_phone: non_empty(&phone.get_untracked()),
      maybe_time_zone: non_empty(&time_zone.get_untracked()),
      maybe_working_hours,
    };
    is_saving.set(true);
    leptos::task::spawn_local(async move {
      match hub.save(request).await {
        Ok(()) => state.close_overlay(),
        Err(failure) => error.set_if_alive(Some(failure.to_string())),
      }
      is_saving.set_if_alive(false);
    });
  };

  let pick_avatar = move |ev: leptos::ev::Event| {
    let Some(input) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
      return;
    };
    let Some(file) = input.files().and_then(|files| files.item(0)) else {
      return;
    };
    input.set_value("");
    let Some(content_type) = avatar_type(&file.type_()) else {
      error.set(Some(i18n.t("profile-avatar-type")));
      return;
    };
    if file.size() as u64 > MAX_AVATAR_BYTES {
      error.set(Some(i18n.t("profile-avatar-size")));
      return;
    }
    error.set(None);
    leptos::task::spawn_local(async move {
      let Ok(buffer) = JsFuture::from(file.array_buffer()).await else {
        error.set_if_alive(Some(i18n.t("profile-avatar-unreadable")));
        return;
      };
      let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
      if let Err(failure) = hub.upload_avatar(me.id, content_type, bytes).await {
        error.set_if_alive(Some(failure.to_string()));
      }
    });
  };
  let remove_avatar = move |_| {
    leptos::task::spawn_local(async move {
      if let Err(failure) = hub.remove_avatar(me.id).await {
        error.set_if_alive(Some(failure.to_string()));
      }
    });
  };

  view! {
    <Modal labelled_by="profile-title">
      <form on:submit=save>
        <header class="modal-head">
          <h2 id="profile-title">{move || i18n.t("profile-edit-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <div class="profile-avatar-row">
            <Avatar user=me.clone() size=AvatarSize::Large />
            <label class="btn btn-small">
              {move || i18n.t("profile-avatar-change")}
              <input type="file" class="visually-hidden" accept="image/png,image/jpeg,image/webp" on:change=pick_avatar />
            </label>
            <Show when=move || hub.profile(me.id).is_some_and(|profile| profile.has_avatar) fallback=|| ()>
              <button type="button" class="btn btn-small" on:click=remove_avatar>{move || i18n.t("profile-avatar-remove")}</button>
            </Show>
          </div>
          <label for="profile-title-field">{move || i18n.t("profile-title")}</label>
          <input id="profile-title-field" class="input" maxlength="100" prop:value=move || title.get() on:input=move |ev| title.set(event_target_value(&ev)) />
          <label for="profile-department">{move || i18n.t("profile-department")}</label>
          <input id="profile-department" class="input" maxlength="100" prop:value=move || department.get() on:input=move |ev| department.set(event_target_value(&ev)) />
          <label for="profile-phone">{move || i18n.t("profile-phone")}</label>
          <input id="profile-phone" class="input" type="tel" maxlength="40" prop:value=move || phone.get() on:input=move |ev| phone.set(event_target_value(&ev)) />
          <label for="profile-zone">{move || i18n.t("profile-time-zone")}</label>
          <input id="profile-zone" class="input" list="profile-zones" autocomplete="off" prop:value=move || time_zone.get() on:input=move |ev| time_zone.set(event_target_value(&ev)) />
          <datalist id="profile-zones">
            {zones.into_iter().map(|zone| view! { <option value=zone></option> }).collect_view()}
          </datalist>
          <label class="check-row">
            <input type="checkbox" prop:checked=move || has_hours.get() on:change=move |ev| has_hours.set(event_target_checked(&ev)) />
            <span>{move || i18n.t("profile-hours")}</span>
          </label>
          <Show when=move || has_hours.get() fallback=|| ()>
            <fieldset class="weekday-picker">
              <legend class="visually-hidden">{move || i18n.t("profile-hours-days")}</legend>
              {weekdays()
                .into_iter()
                .map(|day| {
                  let key = format!("weekday-{}", day.as_str());
                  let day_on_change = day.clone();
                  let day_checked = day.clone();
                  view! {
                    <label class="check-row">
                      <input
                        type="checkbox"
                        prop:checked=move || days.with(|list| list.contains(&day_checked))
                        on:change=move |ev| {
                          let on = event_target_checked(&ev);
                          days.update(|list| {
                            list.retain(|candidate| *candidate != day_on_change);
                            if on {
                              list.push(day_on_change.clone());
                            }
                          });
                        }
                      />
                      <span>{move || i18n.t(&key)}</span>
                    </label>
                  }
                })
                .collect_view()}
            </fieldset>
            <div class="hours-row">
              <label for="profile-start">{move || i18n.t("profile-hours-from")}</label>
              <input id="profile-start" class="input" type="time" prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev)) />
              <label for="profile-end">{move || i18n.t("profile-hours-to")}</label>
              <input id="profile-end" class="input" type="time" prop:value=move || end.get() on:input=move |ev| end.set(event_target_value(&ev)) />
            </div>
          </Show>
          {move || error.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || is_saving.get()>{move || i18n.t("save")}</button>
        </footer>
      </form>
    </Modal>
  }
  .into_any()
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
          .then(|| view! { <button class="btn" on:click=move |_| state.overlay.set(crate::state::Overlay::EditProfile)>{move || i18n.t("profile-edit-title")}</button> })}
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
  let days: Vec<String> = weekdays()
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
