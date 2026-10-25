//! Settings > Account: the profile header (picture, name, user ID) and the profile details others
//! see on the user's card (title, department, phone, time zone, working hours).

use leptos::prelude::*;
use teamcraft_proto::{AvatarContentType, ErrorCode, UpdateProfileRequest, WorkingHours};
use wasm_bindgen::JsCast;

use crate::clock::{local_time_zone, time_zone_names};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::image_crop_dialog::{
  CropTarget, CropUpload, ImageCropDialog, allow_image_drop, dropped_file,
};
use crate::components::message_actions::copy_and_confirm;
use crate::components::pending_button::PendingButton;
use crate::components::profile_views::{Weekday, format_minutes, parse_minutes};
use crate::components::settings_rows::{SettingRow, SettingsCard, SwitchRow, row_id};
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::image_crop::check_pick;
use crate::prefs::EVERY_DAY;
use crate::profiles::{MAX_AVATAR_BYTES, ProfileHub, use_profiles};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

const DEFAULT_START: u16 = 9 * 60;
const DEFAULT_END: u16 = 17 * 60;

#[component]
pub fn AccountSection() -> impl IntoView {
  let i18n = use_i18n();
  let Some(hub) = use_profiles() else {
    return view! {
      <ProfileHeader maybe_hub=None />
      <p class="set-note">{move || i18n.t("profile-unavailable")}</p>
    }
    .into_any();
  };
  view! {
    <ProfileHeader maybe_hub=Some(hub) />
    <ProfileDetails hub=hub />
  }
  .into_any()
}

/// Picture with change and remove, the name and the user ID with a copy button.
#[component]
fn ProfileHeader(maybe_hub: Option<ProfileHub>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me();
  let me_id = me.id;
  let handle = format!("@{}", me.username);
  let copied_handle = handle.clone();
  let picked = RwSignal::new_local(None::<web_sys::File>);
  let pick_error = RwSignal::new(None::<&'static str>);
  let removing = PendingAction::new();

  let open_crop = move |file: web_sys::File| match check_pick(&file.type_(), file.size()) {
    Ok(()) => {
      pick_error.set(None);
      picked.set(Some(file));
    },
    Err(error) => pick_error.set(Some(error.i18n_key())),
  };
  let pick_avatar = move |ev: leptos::ev::Event| {
    let Some(input) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
      return;
    };
    let maybe_file = input.files().and_then(|files| files.item(0));
    input.set_value("");
    if let Some(file) = maybe_file {
      open_crop(file);
    }
  };
  let apply_avatar = Callback::new(move |png: Vec<u8>| -> CropUpload {
    Box::pin(async move {
      let Some(hub) = maybe_hub else {
        return Err(ActionFailure::new(
          FailureKind::Other,
          "profiles are not available",
        ));
      };
      if png.len() as u64 > MAX_AVATAR_BYTES {
        return Err(ActionFailure::new(
          FailureKind::Server(ErrorCode::PayloadTooLarge),
          "cropped avatar above the upload limit",
        ));
      }
      hub
        .upload_avatar(me_id, AvatarContentType::Png, png)
        .await
        .map_err(|failure| failure.to_failure())?;
      picked.set_if_alive(None);
      Ok(())
    })
  });
  let cancel_crop = Callback::new(move |()| picked.set(None));
  let remove_avatar = move |_| {
    if let Some(hub) = maybe_hub {
      removing.spawn(async move {
        hub
          .remove_avatar(me_id)
          .await
          .map_err(|failure| failure.to_failure())
      });
    }
  };
  let has_avatar = move || {
    maybe_hub
      .and_then(|hub| hub.profile(me_id))
      .is_some_and(|profile| profile.has_avatar)
  };

  view! {
    <header
      class="set-profile"
      on:dragover=allow_image_drop
      on:drop=move |ev| {
        if maybe_hub.is_some()
          && let Some(file) = dropped_file(&ev)
        {
          open_crop(file);
        }
      }
    >
      <div class="set-profile-avatar">
        <Avatar user=me.clone() size=AvatarSize::Large />
      </div>
      <div class="set-profile-text">
        <h3 class="set-profile-name" translate="no">{me.display_name.clone()}</h3>
        <div class="set-profile-handle">
          <span class="mono" translate="no">{handle}</span>
          <button
            type="button"
            class="icon-btn icon-btn-small"
            aria-label=move || i18n.t("settings-copy-username")
            title=move || i18n.t("settings-copy-username")
            on:click=move |_| copy_and_confirm(copied_handle.clone())
          >
            <Icon name=IconName::Copy />
          </button>
        </div>
        <p class="set-row-hint">{move || i18n.t("settings-name-from-sign-in")}</p>
        {maybe_hub.is_some().then(|| view! {
          <div class="set-profile-actions">
            <label class="btn btn-small" id=row_id("profile-avatar-change")>
              <Icon name=IconName::Camera />
              <span>{move || i18n.t("profile-avatar-change")}</span>
              <input type="file" class="visually-hidden" accept="image/png,image/jpeg,image/webp" on:change=pick_avatar />
            </label>
            <Show when=has_avatar fallback=|| ()>
              <PendingButton pending=removing class="btn btn-small" on_click=remove_avatar>
                {move || i18n.t("profile-avatar-remove")}
              </PendingButton>
            </Show>
          </div>
        })}
        {move || pick_error.get().map(|key| view! { <p class="field-error" role="alert">{move || i18n.t(key)}</p> })}
      </div>
    </header>
    {move || picked.get().map(|file| view! {
      <ImageCropDialog file=file target=CropTarget::Avatar on_apply=apply_avatar on_cancel=cancel_crop />
    })}
  }
}

/// What others see on the user's card. Saved together, with a clear saved or failed state.
#[component]
fn ProfileDetails(hub: ProfileHub) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me().id;
  let title = RwSignal::new(String::new());
  let department = RwSignal::new(String::new());
  let phone = RwSignal::new(String::new());
  let time_zone = RwSignal::new(String::new());
  let has_hours = RwSignal::new(false);
  let days = RwSignal::new(EVERY_DAY[..5].to_vec());
  let start = RwSignal::new(format_minutes(DEFAULT_START));
  let end = RwSignal::new(format_minutes(DEFAULT_END));
  let is_hours_invalid = RwSignal::new(false);
  let is_saved = RwSignal::new(false);
  let saving = PendingAction::new();
  let zones = time_zone_names();

  // The stored profile arrives after the section opened: fill the form once.
  let is_filled = RwSignal::new(false);
  Effect::new(move |_| {
    if is_filled.get_untracked() {
      return;
    }
    let Some(profile) = hub.profile(me) else { return };
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
  // Any edit after saving takes the "Saved" away again.
  Effect::new(move |_| {
    title.track();
    department.track();
    phone.track();
    time_zone.track();
    has_hours.track();
    days.track();
    start.track();
    end.track();
    is_saved.set(false);
  });

  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let maybe_working_hours = if has_hours.get_untracked() {
      let Some(hours) = working_hours(
        &days.get_untracked(),
        &start.get_untracked(),
        &end.get_untracked(),
      ) else {
        is_hours_invalid.set(true);
        return;
      };
      Some(hours)
    } else {
      None
    };
    is_hours_invalid.set(false);
    let request = UpdateProfileRequest {
      maybe_title: non_empty(&title.get_untracked()),
      maybe_department: non_empty(&department.get_untracked()),
      maybe_phone: non_empty(&phone.get_untracked()),
      maybe_time_zone: non_empty(&time_zone.get_untracked()),
      maybe_working_hours,
    };
    saving.spawn(async move {
      hub.save(request).await.map_err(|failure| failure.to_failure())?;
      is_saved.set_if_alive(true);
      Ok(())
    });
  };

  let text_field =
    move |id: &'static str, label: &'static str, value: RwSignal<String>, kind: &'static str| {
      view! {
        <div class="set-row is-field" id=row_id(label)>
          <label class="set-row-label" for=id>{move || i18n.t(label)}</label>
          <input
            id=id
            class="input"
            type=kind
            maxlength=if kind == "tel" { "40" } else { "100" }
            autocomplete=if kind == "tel" { "tel" } else { "off" }
            prop:value=move || value.get()
            on:input=move |ev| value.set(event_target_value(&ev))
          />
        </div>
      }
    };

  view! {
    <form class="set-form" on:submit=save>
      <SettingsCard heading="settings-profile-details" hint="settings-profile-details-hint">
        {text_field("profile-title-field", "profile-title", title, "text")}
        {text_field("profile-department", "profile-department", department, "text")}
        {text_field("profile-phone", "profile-phone", phone, "tel")}
        <div class="set-row is-field" id=row_id("profile-time-zone")>
          <label class="set-row-label" for="profile-zone">{move || i18n.t("profile-time-zone")}</label>
          <input
            id="profile-zone"
            class="input"
            list="profile-zones"
            autocomplete="off"
            prop:value=move || time_zone.get()
            on:input=move |ev| time_zone.set(event_target_value(&ev))
          />
          <datalist id="profile-zones">
            {zones.into_iter().map(|zone| view! { <option value=zone></option> }).collect_view()}
          </datalist>
        </div>
      </SettingsCard>
      <SettingsCard heading="profile-hours">
        <SwitchRow
          label="settings-profile-hours-show"
          hint="settings-profile-hours-show-hint"
          value=has_hours
          on_change=Callback::new(move |on: bool| has_hours.set(on))
        />
        <Show when=move || has_hours.get() fallback=|| ()>
          <SettingRow label="profile-hours-days" is_stacked=true>
            <WeekdayChips days=days />
          </SettingRow>
          <div class="set-row is-field set-hours">
            <label class="set-row-label" for="profile-start">{move || i18n.t("profile-hours-from")}</label>
            <input id="profile-start" class="input" type="time" prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev)) />
            <label class="set-row-label" for="profile-end">{move || i18n.t("profile-hours-to")}</label>
            <input id="profile-end" class="input" type="time" prop:value=move || end.get() on:input=move |ev| end.set(event_target_value(&ev)) />
          </div>
          <Show when=move || is_hours_invalid.get() fallback=|| ()>
            <p class="field-error" role="alert">{move || i18n.t("profile-hours-invalid")}</p>
          </Show>
        </Show>
      </SettingsCard>
      <div class="set-form-foot">
        <Show when=move || is_saved.get() fallback=|| ()>
          <span class="set-saved" role="status"><Icon name=IconName::Check />{move || i18n.t("settings-saved")}</span>
        </Show>
        <button type="submit" class="btn btn-primary" prop:disabled=move || saving.is_pending() aria-busy=move || saving.is_pending().then_some("true")>
          <Show when=move || saving.is_pending() fallback=|| ()>
            <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
          </Show>
          {move || i18n.t("settings-profile-save")}
        </button>
      </div>
    </form>
  }
}

/// The weekdays as toggle chips (`aria-pressed`), in a row that wraps.
#[component]
pub fn WeekdayChips(
  days: RwSignal<Vec<Weekday>>,
  #[prop(optional, into)] is_disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="set-chips" role="group">
      {EVERY_DAY
        .into_iter()
        .map(|day| {
          let key = format!("weekday-{}", day.as_str());
          let is_on_day = day.clone();
          let toggle_day = day.clone();
          let is_on = move || days.with(|list| list.contains(&is_on_day));
          view! {
            <button
              type="button"
              class="set-chip"
              aria-pressed=move || is_on().to_string()
              prop:disabled=move || is_disabled.get()
              on:click=move |_| {
                let day = toggle_day.clone();
                days.update(|list| {
                  if list.contains(&day) {
                    list.retain(|candidate| *candidate != day);
                  } else {
                    list.push(day);
                  }
                });
              }
            >
              {move || i18n.t(&key)}
            </button>
          }
        })
        .collect_view()}
    </div>
  }
}

/// The working hours of the form, if they make sense: a day and an end after the start.
fn working_hours(days: &[Weekday], start: &str, end: &str) -> Option<WorkingHours> {
  let (start_minute, end_minute) = (parse_minutes(start)?, parse_minutes(end)?);
  (end_minute > start_minute && !days.is_empty()).then(|| WorkingHours {
    weekdays: days.to_vec(),
    start_minute,
    end_minute,
  })
}

fn non_empty(text: &str) -> Option<String> {
  let trimmed = text.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn working_hours_need_a_day_and_an_end_after_the_start() {
    let monday = [Weekday::Monday];
    assert!(working_hours(&monday, "09:00", "17:00").is_some());
    assert!(working_hours(&monday, "17:00", "09:00").is_none());
    assert!(working_hours(&[], "09:00", "17:00").is_none());
    assert!(working_hours(&monday, "9", "17:00").is_none());
  }

  #[test]
  fn blank_fields_are_cleared() {
    assert_eq!(non_empty("  "), None);
    assert_eq!(non_empty(" Sales "), Some("Sales".to_owned()));
  }
}
