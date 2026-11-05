//! The notice before guests lose their access (issue #212): to the guest themself, and to the sponsor
//! and the space's admins with "extend", where the server says they may. Above the app, like the
//! announcement; it stands until the expiry changes or passes, and "dismiss" lasts until the list
//! changes.

use leptos::prelude::*;
use teamcraft_proto::ExpiringGuestResponse;

use crate::clock::format_date;
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// How far the notice offers to extend, in days.
const EXTEND_DAYS: [i64; 2] = [30, 90];
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[component]
pub fn GuestNotice() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let guests = Memo::new(move |_| state.read(|backend| backend.expiring_guests()));
  let dismissed = RwSignal::new(None::<Vec<ExpiringGuestResponse>>);
  let is_open = RwSignal::new(false);
  let own = Memo::new(move |_| {
    guests
      .get()
      .into_iter()
      .filter(|guest| guest.is_own)
      .collect::<Vec<_>>()
  });
  let others = Memo::new(move |_| {
    guests
      .get()
      .into_iter()
      .filter(|guest| !guest.is_own)
      .collect::<Vec<_>>()
  });
  let is_shown =
    move || !guests.with(Vec::is_empty) && dismissed.with(|list| list.as_ref() != Some(&guests.get()));
  move || {
    is_shown().then(|| {
      view! {
        <div class="announcement-banner announcement-info guest-notice" role="status">
          <Icon name=IconName::Alert />
          <div class="announcement-text">
            <For each=move || own.get() key=|guest| (guest.space_id, guest.expires_at) let:guest>
              <p translate="no">
                {move || i18n.t_with("guest-notice-self", &[
                  ("space", &guest.space_name),
                  ("date", &format_date(guest.expires_at.0, i18n.locale().tag())),
                ])}
              </p>
            </For>
            <Show when=move || !others.with(Vec::is_empty) fallback=|| ()>
              <p>
                {move || i18n.t_count("guest-notice-others", others.with(Vec::len))}
                " "
                <button
                  type="button"
                  class="btn btn-small"
                  aria-expanded=move || is_open.get().to_string()
                  on:click=move |_| is_open.update(|open| *open = !*open)
                >
                  {move || i18n.t(if is_open.get() { "guest-notice-hide" } else { "guest-notice-show" })}
                </button>
              </p>
              <Show when=move || is_open.get() fallback=|| ()>
                <ul class="guest-notice-list">
                  <For each=move || others.get() key=|guest| (guest.space_id, guest.user.id, guest.expires_at) let:guest>
                    <GuestNoticeRow guest=guest />
                  </For>
                </ul>
              </Show>
            </Show>
          </div>
          <button
            class="icon-button"
            aria-label=move || i18n.t("announcement-dismiss")
            on:click=move |_| dismissed.set(Some(guests.get_untracked()))
          >
            <Icon name=IconName::Close />
          </button>
        </div>
      }
    })
  }
}

#[component]
fn GuestNoticeRow(guest: ExpiringGuestResponse) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let (space, user, expires_at) = (guest.space_id, guest.user.id, guest.expires_at.0);
  let date = move || format_date(expires_at, i18n.locale().tag());
  let name = guest.user.display_name.clone();
  let space_name = guest.space_name.clone();
  let options: Vec<i64> = extend_options(&guest);
  view! {
    <li class="guest-notice-row">
      <span translate="no">
        {move || i18n.t_with("guest-notice-row", &[("name", &name), ("space", &space_name), ("date", &date())])}
      </span>
      {options
        .into_iter()
        .map(|days| {
          view! {
            <button
              type="button"
              class="btn btn-small"
              prop:disabled=move || pending.is_pending()
              on:click=move |_| {
                pending.run(|| state.write(|backend| backend.extend_guest(space, user, expires_at + days * DAY_MS)));
              }
            >
              {move || i18n.t_count("guest-notice-extend", days as usize)}
            </button>
          }
        })
        .collect_view()}
    </li>
  }
}

/// The offers the user may take: nothing without the right, nothing past their limit.
fn extend_options(guest: &ExpiringGuestResponse) -> Vec<i64> {
  if !guest.can_extend {
    return Vec::new();
  }
  EXTEND_DAYS
    .into_iter()
    .filter(|days| {
      guest
        .maybe_extend_limit
        .is_none_or(|limit| guest.expires_at.0 + days * DAY_MS <= limit.0)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::UnixMillis;

  use super::*;

  #[test]
  fn only_what_the_server_allows_is_offered() {
    assert_eq!(extend_options(&guest(false, None)), Vec::<i64>::new());
    assert_eq!(extend_options(&guest(true, None)), vec![30, 90]);
    assert_eq!(extend_options(&guest(true, Some(40))), vec![30]);
    assert_eq!(extend_options(&guest(true, Some(10))), Vec::<i64>::new());
  }

  fn guest(can_extend: bool, maybe_limit_days: Option<i64>) -> ExpiringGuestResponse {
    ExpiringGuestResponse {
      space_id: "1".parse().unwrap(),
      space_name: "Project A".to_owned(),
      user: serde_json::from_value(serde_json::json!({"id": "2", "display_name": "Guest", "kind": "guest"}))
        .unwrap(),
      expires_at: UnixMillis(0),
      is_own: false,
      can_extend,
      maybe_extend_limit: maybe_limit_days.map(|days| UnixMillis(days * DAY_MS)),
    }
  }
}
