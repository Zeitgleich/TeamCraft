use leptos::prelude::*;
use teamcraft_proto::{AnnouncementLevel, SetAnnouncementRequest, UnixMillis};

use crate::clock::{format_date_time, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::admin_policy::SelectRow;
use crate::components::announcement_banner::BannerView;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const MAX_TEXT_CHARS: usize = 500;
/// Expiry choices in hours; `0` keeps the banner until somebody clears it.
const EXPIRY_HOURS: [u32; 5] = [0, 1, 24, 7 * 24, 30 * 24];

/// The announcement banner every signed-in client shows above the app until it is dismissed.
#[component]
pub fn AnnouncementSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = Memo::new(move |_| state.read(|backend| backend.announcement()));
  let text = RwSignal::new(String::new());
  let level = RwSignal::new(AnnouncementLevel::Info);
  let is_dismissible = RwSignal::new(true);
  let expiry_hours = RwSignal::new(0_u32);
  // Start the form from the banner in force once it is known.
  let seeded = RwSignal::new(false);
  Effect::new(move |_| {
    if let Some(announcement) = current.get()
      && !seeded.get_untracked()
    {
      seeded.set(true);
      text.set(announcement.text);
      level.set(announcement.level);
      is_dismissible.set(announcement.is_dismissible);
    }
  });

  let level_options = Signal::derive(move || {
    ["info", "warning", "critical"]
      .into_iter()
      .map(|slug| (slug.to_owned(), i18n.t(&format!("announcement-level-{slug}"))))
      .collect()
  });
  let expiry_options = Signal::derive(move || {
    EXPIRY_HOURS
      .into_iter()
      .map(|hours| {
        let label = match hours {
          0 => i18n.t("announcement-no-expiry"),
          1 => i18n.t("announcement-1h"),
          24 => i18n.t("announcement-1d"),
          hours => i18n.t_count("policy-days", (hours / 24) as usize),
        };
        (hours.to_string(), label)
      })
      .collect()
  });
  let is_valid = move || {
    let length = text.get().trim().chars().count();
    length > 0 && length <= MAX_TEXT_CHARS
  };
  let publish = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !is_valid() {
      return;
    }
    let hours = expiry_hours.get_untracked();
    let request = SetAnnouncementRequest {
      text: text.get_untracked().trim().to_owned(),
      level: level.get_untracked(),
      is_dismissible: is_dismissible.get_untracked(),
      maybe_expires_at: (hours > 0).then(|| UnixMillis(now_ms() + i64::from(hours) * 3_600_000)),
    };
    state.write(|backend| backend.set_announcement(request));
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-announcement-title"))
      subtitle=Signal::derive(move || i18n.t("admin-announcement-subtitle"))
    />
    <div class="announcement-admin">
      <section class="announcement-state" aria-live="polite">
        <h3>{move || i18n.t("announcement-current")}</h3>
        {move || match current.get() {
          Some(announcement) => {
            let expires = announcement
              .maybe_expires_at
              .map(|at| i18n.t_with("announcement-expires", &[("time", &format_date_time(at.0, i18n.locale().tag()))]));
            view! {
              <BannerView text=announcement.text.clone() level=announcement.level.clone() is_dismissible=announcement.is_dismissible />
              <p class="muted-text small">{expires.unwrap_or_else(|| i18n.t("announcement-no-expiry"))}</p>
            }
              .into_any()
          },
          None => view! { <p class="muted-text">{move || i18n.t("announcement-none")}</p> }.into_any(),
        }}
      </section>
      <form class="policy-form" on:submit=publish>
        <fieldset class="policy-group">
          <legend>{move || i18n.t("announcement-edit")}</legend>
          <div class="field">
            <label for="announcement-text">{move || i18n.t("announcement-text")}</label>
            <textarea
              id="announcement-text"
              class="input"
              rows="3"
              maxlength=MAX_TEXT_CHARS.to_string()
              prop:value=move || text.get()
              on:input=move |ev| text.set(event_target_value(&ev))
            ></textarea>
            <p class="muted-text small">
              {move || i18n.t_with("announcement-count", &[("count", &text.get().chars().count().to_string()), ("max", &MAX_TEXT_CHARS.to_string())])}
            </p>
          </div>
          <SelectRow
            id="announcement-level"
            label=Signal::derive(move || i18n.t("announcement-level"))
            options=level_options
            selected=Signal::derive(move || level.get().as_str().to_owned())
            on_select=move |value: String| {
              level.set(match value.as_str() {
                "warning" => AnnouncementLevel::Warning,
                "critical" => AnnouncementLevel::Critical,
                _ => AnnouncementLevel::Info,
              });
            }
          />
          <SelectRow
            id="announcement-expiry"
            label=Signal::derive(move || i18n.t("announcement-expiry"))
            options=expiry_options
            selected=Signal::derive(move || expiry_hours.get().to_string())
            on_select=move |value: String| expiry_hours.set(value.parse().unwrap_or(0))
          />
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=move || is_dismissible.get()
              on:change=move |ev| is_dismissible.set(event_target_checked(&ev))
            />
            <span>{move || i18n.t("announcement-dismissible")}</span>
          </label>
          <h4 class="detail-heading">{move || i18n.t("announcement-preview")}</h4>
          <BannerView
            text=Signal::derive(move || text.get())
            level=Signal::derive(move || level.get())
            is_dismissible=Signal::derive(move || is_dismissible.get())
          />
        </fieldset>
        <footer class="policy-footer">
          <button
            type="button"
            class="btn"
            disabled=move || current.get().is_none()
            on:click=move |_| state.write(|backend| backend.clear_announcement())
          >
            {move || i18n.t("announcement-clear")}
          </button>
          <button type="submit" class="btn btn-primary" disabled=move || !is_valid()>
            {move || i18n.t("announcement-publish")}
          </button>
        </footer>
      </form>
    </div>
  }
}
