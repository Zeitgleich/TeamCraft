//! The notice before guests lose their access (issue #212): to the guest themself, and to the sponsor
//! and the space's admins with "extend", where the server says they may. Above the app, like the
//! announcement; it stands until the expiry changes or passes, and "dismiss" lasts until the list
//! changes.

use leptos::prelude::*;
use teamcraft_proto::{ExpiringGuestResponse, GuestExtendOffer, SpaceId, UnixMillis, UserId};

use crate::clock::format_date;
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

#[component]
pub fn GuestNotice() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let guests = Memo::new(move |_| state.read(|backend| backend.expiring_guests()));
  let dismissed = RwSignal::new(None::<Vec<NoticeKey>>);
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
  let is_shown = move || {
    !guests.with(Vec::is_empty) && dismissed.with(|list| list.as_ref() != Some(&notice_keys(&guests.get())))
  };
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
            on:click=move |_| dismissed.set(Some(notice_keys(&guests.get_untracked())))
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
  let (space, user) = (guest.space_id, guest.user.id);
  let expires_at = guest.expires_at.0;
  let date = move || format_date(expires_at, i18n.locale().tag());
  let name = guest.user.display_name.clone();
  let space_name = guest.space_name.clone();
  view! {
    <li class="guest-notice-row">
      <span translate="no">
        {move || i18n.t_with("guest-notice-row", &[("name", &name), ("space", &space_name), ("date", &date())])}
      </span>
      {guest
        .extend_offers
        .into_iter()
        .map(|offer: GuestExtendOffer| {
          let until = offer.until.0;
          let label = move || match offer.maybe_days {
            Some(days) => i18n.t_count("guest-notice-extend", days as usize),
            None => i18n.t_with("guest-notice-extend-until", &[("date", &format_date(until, i18n.locale().tag()))]),
          };
          view! {
            <button
              type="button"
              class="btn btn-small"
              prop:disabled=move || pending.is_pending()
              on:click=move |_| {
                pending.run(|| state.write(|backend| backend.extend_guest(space, user, until)));
              }
            >
              {label}
            </button>
          }
        })
        .collect_view()}
    </li>
  }
}

/// What a dismissal holds for: a notice is the same while its expiry is, whatever the offers say.
type NoticeKey = (SpaceId, UserId, UnixMillis);

fn notice_keys(guests: &[ExpiringGuestResponse]) -> Vec<NoticeKey> {
  guests
    .iter()
    .map(|guest| (guest.space_id, guest.user.id, guest.expires_at))
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::GuestExtendOffer;

  use super::*;

  #[test]
  fn a_dismissal_holds_when_only_the_offers_change() {
    let mut guest: ExpiringGuestResponse = serde_json::from_value(serde_json::json!({
      "space_id": "1", "space_name": "Project A", "expires_at": 1000, "is_own": false,
      "user": {"id": "2", "display_name": "Guest", "kind": "guest"},
    }))
    .unwrap();
    let before = notice_keys(std::slice::from_ref(&guest));
    guest.extend_offers.push(GuestExtendOffer {
      until: UnixMillis(5000),
      maybe_days: None,
    });
    assert_eq!(notice_keys(std::slice::from_ref(&guest)), before);
    guest.expires_at = UnixMillis(2000);
    assert_ne!(
      notice_keys(std::slice::from_ref(&guest)),
      before,
      "a new expiry shows again"
    );
  }
}
