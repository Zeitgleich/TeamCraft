//! Settings > Account: the profile header (picture, name, user ID) and the profile details others
//! see on the user's card (title, department, phone, time zone, working hours).

use leptos::prelude::*;
use teamcraft_proto::{AvatarContentType, ErrorCode, ProfileResponse, UpdateProfileRequest, WorkingHours};
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
use crate::form_errors::{FieldError, FieldErrors, FormErrors, focus_first_invalid};
use crate::i18n::use_i18n;
use crate::image_crop::check_pick;
use crate::prefs::EVERY_DAY;
use crate::profiles::{MAX_AVATAR_BYTES, ProfileHub, use_profiles};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

const PROFILE_FORM_ID: &str = "profile-form";
const HOURS_ERROR_ID: &str = "profile-hours-error";
const NAME_FORM_ID: &str = "profile-name-form";
const NAME_FIELD_ID: &str = "profile-name-field";
/// Server refusals that belong to the name field: a name that is too long or has characters the
/// server does not accept, and a policy that forbids changing it.
const NAME_SERVER_FIELDS: [(ErrorCode, &str); 2] = [
  (ErrorCode::InvalidRequest, NAME_FIELD_ID),
  (ErrorCode::DisplayNameOverrideNotAllowed, NAME_FIELD_ID),
];
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
        <h3 class="set-profile-name" translate="no">{move || state.me().display_name}</h3>
        {maybe_hub.map(|hub| view! { <DisplayNameEditor hub=hub /> })}
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
        <Show when=move || !can_edit_name(maybe_hub.and_then(|hub| hub.profile(me_id)).as_ref()) fallback=|| ()>
          <p class="set-row-hint">{move || i18n.t("settings-name-from-sign-in")}</p>
        </Show>
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

/// Changes the name under the picture, when the workspace policy allows it (issue #408): a button
/// that opens a field, and "Use name from sign-in" while a name of the user's own is set. Saved
/// with the rest of the profile, which the update replaces as a whole.
#[component]
fn DisplayNameEditor(hub: ProfileHub) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me().id;
  let is_editing = RwSignal::new(false);
  let name = RwSignal::new(String::new());
  let errors = FormErrors::new();
  let saving = PendingAction::new();
  let resetting = PendingAction::new();
  let stored = move || hub.profile(me);
  let is_overridden = move || {
    stored()
      .and_then(|profile| profile.maybe_display_name_override)
      .is_some()
  };
  let field_errors = move || {
    let mut found = FieldErrors::new();
    if let Some(key) = name_error(&name.get()) {
      found.push(NAME_FIELD_ID, i18n.t(key));
    }
    found
  };
  Effect::new(move |_| errors.follow(field_errors()));

  let open = move |_| {
    name.set(state.me().display_name);
    errors.clear();
    is_editing.set(true);
    focus_element(NAME_FIELD_ID);
  };
  let close = move |_| {
    is_editing.set(false);
    errors.clear();
  };
  // Sends the profile as stored with only the name changed; `None` goes back to the sign-in name.
  let send = move |maybe_name: Option<String>, action: PendingAction| {
    let Some(profile) = stored() else { return };
    let mut request = UpdateProfileRequest::from(&profile);
    request.maybe_display_name = maybe_name;
    action.spawn(async move {
      match hub.save(request).await {
        Ok(()) => {
          is_editing.set_if_alive(false);
          errors.clear();
          Ok(())
        },
        Err(refusal) => {
          let failure = refusal.to_failure();
          let message = i18n.t(name_refusal_key(&failure));
          // A refusal of the name goes to its field; the toast is for the rest.
          if errors.refuse_server(NAME_FORM_ID, &failure, &NAME_SERVER_FIELDS, message) {
            Ok(())
          } else {
            Err(failure)
          }
        },
      }
    });
  };
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(NAME_FORM_ID, found);
      return;
    }
    send(Some(name.get_untracked().trim().to_owned()), saving);
  };

  view! {
    <Show when=move || can_edit_name(stored().as_ref()) fallback=|| ()>
      <Show
        when=move || is_editing.get()
        fallback=move || view! {
          <div class="set-profile-actions">
            <button type="button" class="btn btn-small" on:click=open>
              {move || i18n.t("profile-name-change")}
            </button>
            <Show when=is_overridden fallback=|| ()>
              <PendingButton pending=resetting class="btn btn-small" on_click=move |_| send(None, resetting)>
                {move || i18n.t("profile-name-reset")}
              </PendingButton>
            </Show>
          </div>
        }
      >
        <form id=NAME_FORM_ID class="set-name-form" on:submit=save>
          <label class="set-row-label" for=NAME_FIELD_ID>{move || i18n.t("profile-name-label")}</label>
          <input
            id=NAME_FIELD_ID
            class="input"
            type="text"
            autocomplete="name"
            aria-invalid=errors.invalid(NAME_FIELD_ID)
            aria-describedby=errors.described(NAME_FIELD_ID)
            prop:value=move || name.get()
            on:input=move |ev| {
              errors.clear_field(NAME_FIELD_ID);
              name.set(event_target_value(&ev));
            }
          />
          <FieldError errors=errors field=NAME_FIELD_ID />
          <div class="set-profile-actions">
            <button type="submit" class="btn btn-small btn-primary" prop:disabled=move || saving.is_pending() aria-busy=move || saving.is_pending().then_some("true")>
              {move || i18n.t("profile-name-save")}
            </button>
            <button type="button" class="btn btn-small" on:click=close>{move || i18n.t("cancel")}</button>
            <Show when=is_overridden fallback=|| ()>
              <PendingButton pending=resetting class="btn btn-small" on_click=move |_| send(None, resetting)>
                {move || i18n.t("profile-name-reset")}
              </PendingButton>
            </Show>
          </div>
        </form>
      </Show>
    </Show>
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

  let hours_invalid = move || is_hours_invalid.get().then_some("true");
  let hours_described = move || is_hours_invalid.get().then_some(HOURS_ERROR_ID);
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let maybe_working_hours = if has_hours.get_untracked() {
      let Some(hours) = working_hours(
        &days.get_untracked(),
        &start.get_untracked(),
        &end.get_untracked(),
      ) else {
        is_hours_invalid.set(true);
        focus_first_invalid(PROFILE_FORM_ID);
        return;
      };
      Some(hours)
    } else {
      None
    };
    is_hours_invalid.set(false);
    let request = UpdateProfileRequest {
      // This form does not edit the name, and the update replaces everything: keep it.
      maybe_display_name: hub
        .profile(me)
        .and_then(|profile| profile.maybe_display_name_override),
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
    <form id=PROFILE_FORM_ID class="set-form" on:submit=save>
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
            <input id="profile-start" class="input" type="time" aria-invalid=hours_invalid aria-describedby=hours_described prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev)) />
            <label class="set-row-label" for="profile-end">{move || i18n.t("profile-hours-to")}</label>
            <input id="profile-end" class="input" type="time" aria-invalid=hours_invalid aria-describedby=hours_described prop:value=move || end.get() on:input=move |ev| end.set(event_target_value(&ev)) />
          </div>
          <Show when=move || is_hours_invalid.get() fallback=|| ()>
            <p id=HOURS_ERROR_ID class="field-error" role="alert">{move || i18n.t("profile-hours-invalid")}</p>
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

/// Whether the name under the picture can be changed: the profile has loaded and the workspace
/// policy (as the server reports it) allows a name of one's own.
fn can_edit_name(maybe_profile: Option<&ProfileResponse>) -> bool {
  maybe_profile.is_some_and(|profile| profile.can_override_display_name)
}

/// The i18n key of what is wrong with the typed name, if anything. The server checks the length,
/// which is a setting; this catches what is plain from the text.
fn name_error(text: &str) -> Option<&'static str> {
  let trimmed = text.trim();
  if trimmed.is_empty() {
    Some("profile-name-empty")
  } else if trimmed.chars().any(char::is_control) {
    Some("profile-name-invalid")
  } else {
    None
  }
}

/// The message under the name field for a refusal of the server.
fn name_refusal_key(failure: &ActionFailure) -> &'static str {
  match &failure.kind {
    FailureKind::Server(ErrorCode::DisplayNameOverrideNotAllowed) => "profile-name-not-allowed",
    _ => "profile-name-rejected",
  }
}

/// Moves the focus to the element with this id once the view has updated.
fn focus_element(id: &'static str) {
  request_animation_frame(move || {
    if let Some(element) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(id))
      .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
      drop(element.focus());
    }
  });
}

fn non_empty(text: &str) -> Option<String> {
  let trimmed = text.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn working_hours_need_a_day_and_an_end_after_the_start() {
    let monday = [Weekday::Monday];
    assert!(working_hours(&monday, "09:00", "17:00").is_some());
    assert!(working_hours(&monday, "17:00", "09:00").is_none());
    assert!(working_hours(&[], "09:00", "17:00").is_none());
    assert!(working_hours(&monday, "9", "17:00").is_none());
  }

  #[test]
  fn the_name_can_be_changed_only_when_the_policy_allows_it() {
    let mut profile = ProfileResponse {
      user_id: id_from_raw(1),
      maybe_title: None,
      maybe_department: None,
      maybe_phone: None,
      maybe_time_zone: None,
      maybe_working_hours: None,
      has_avatar: false,
      maybe_display_name_override: None,
      can_override_display_name: false,
      maybe_avatar_version: None,
    };
    assert!(!can_edit_name(None), "nothing to edit before the profile loaded");
    assert!(!can_edit_name(Some(&profile)), "the policy forbids it");
    profile.can_override_display_name = true;
    assert!(can_edit_name(Some(&profile)));
  }

  #[test]
  fn a_typed_name_is_checked_before_it_is_sent() {
    assert_eq!(name_error("  "), Some("profile-name-empty"));
    assert_eq!(name_error("Ada\u{7}"), Some("profile-name-invalid"));
    assert_eq!(name_error(" Ada L. "), None);
  }

  #[test]
  fn a_policy_refusal_is_worded_differently_from_a_bad_name() {
    let refused = |code| ActionFailure::new(FailureKind::Server(code), "refused");
    assert_eq!(
      name_refusal_key(&refused(ErrorCode::DisplayNameOverrideNotAllowed)),
      "profile-name-not-allowed"
    );
    assert_eq!(
      name_refusal_key(&refused(ErrorCode::InvalidRequest)),
      "profile-name-rejected"
    );
  }

  #[test]
  fn blank_fields_are_cleared() {
    assert_eq!(non_empty("  "), None);
    assert_eq!(non_empty(" Sales "), Some("Sales".to_owned()));
  }
}
