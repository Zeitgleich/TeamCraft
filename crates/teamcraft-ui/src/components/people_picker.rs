//! Picking several people from the directory (#283): chips for the picked ones, a search field and
//! the matches with avatars. Shared by the invitation, add-to-channel and new-message dialogs, so no
//! dialog picks people from a plain `<select>`.

use std::collections::HashSet;

use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::{DeviceSetup, User, UserId};
use crate::reactive::use_debounced;
use crate::state::use_app_state;

/// Typing asks the directory after this pause.
const DIRECTORY_DEBOUNCE_MS: u64 = 250;

/// What a match in the list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Candidate {
  Pickable,
  /// Picked already (checked; a click takes them back).
  Picked,
  /// Already where the dialog adds people (greyed out, not pickable).
  AlreadyIn,
}

/// `allows_guest_accounts`: whether guest accounts can be picked (not when inviting members).
/// `already_in`: people shown greyed out with `already_in_key` (the members of the channel).
#[component]
pub fn PeoplePicker(
  chosen: RwSignal<Vec<User>>,
  /// Names the list of picked people for screen readers ("Recipients", "People to invite").
  chosen_label_key: &'static str,
  #[prop(into)] allows_guest_accounts: Signal<bool>,
  #[prop(optional, into)] already_in: Signal<HashSet<UserId>>,
  #[prop(optional)] already_in_key: &'static str,
  /// Called on Enter in the empty search field (the dialog's main action).
  #[prop(optional)]
  on_submit: Option<Callback<()>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let directory_query = use_debounced(filter, DIRECTORY_DEBOUNCE_MS);
  let input = NodeRef::<leptos::html::Input>::new();
  let candidates = Memo::new(move |_| {
    let me = state.me().id;
    let picked: HashSet<UserId> = chosen.with(|list| list.iter().map(|user| user.id).collect());
    let users = state.read(|backend| backend.search_people(directory_query.get().trim()));
    candidates(
      users,
      me,
      allows_guest_accounts.get(),
      &picked,
      &already_in.get(),
      &filter.get(),
    )
  });
  let toggle = move |user: User| {
    chosen.update(|list| {
      if let Some(index) = list.iter().position(|picked| picked.id == user.id) {
        list.remove(index);
      } else {
        list.push(user);
      }
    });
  };
  let pick = move |user: User| {
    chosen.update(|list| list.push(user));
    filter.set(String::new());
    if let Some(element) = input.get_untracked() {
      drop(element.focus());
    }
  };
  let on_keydown = move |ev: leptos::ev::KeyboardEvent| {
    let is_filter_empty = filter.get_untracked().trim().is_empty();
    match ev.key().as_str() {
      // Enter picks the first match while searching, and runs the dialog's action otherwise.
      "Enter" => {
        ev.prevent_default();
        let maybe_first = candidates
          .get_untracked()
          .into_iter()
          .find(|(_, kind)| *kind == Candidate::Pickable)
          .map(|(user, _)| user);
        match (is_filter_empty, maybe_first, on_submit) {
          (false, Some(first), _) => pick(first),
          (true, _, Some(submit)) => submit.run(()),
          _ => {},
        }
      },
      // Backspace in the empty field takes back the last person, as in other pickers.
      "Backspace" if is_filter_empty => chosen.update(|list| {
        list.pop();
      }),
      _ => {},
    }
  };
  view! {
    <ul class="chip-list" aria-label=move || i18n.t(chosen_label_key)>
      <For each=move || chosen.get() key=|user| user.id let:user>
        {
          let id = user.id;
          let remove_label = {
            let name = user.display_name.clone();
            move || i18n.t_with("dm-new-remove", &[("name", &name)])
          };
          view! {
            <li class="chip">
              <span translate="no">{user.display_name.clone()}</span>
              <button
                type="button"
                class="icon-btn icon-btn-small"
                aria-label=remove_label.clone()
                title=remove_label
                on:click=move |_| chosen.update(|list| list.retain(|user| user.id != id))
              >
                <Icon name=IconName::Close />
              </button>
            </li>
          }
        }
      </For>
    </ul>
    <label class="visually-hidden" for="people-picker-filter">{move || i18n.t("dm-new-filter")}</label>
    <input
      id="people-picker-filter"
      node_ref=input
      class="input"
      data-autofocus
      autocomplete="off"
      placeholder=move || i18n.t("dm-new-filter")
      prop:value=move || filter.get()
      on:input=move |ev| filter.set(event_target_value(&ev))
      on:keydown=on_keydown
    />
    // A list of checkboxes: each person is a toggle button (role "checkbox", Space or Enter
    // toggles, Tab moves on), which assistive technology announces with its state.
    <ul class="forward-list people-picker" aria-label=move || i18n.t("dm-new-people")>
      <For each=move || candidates.get() key=|(user, kind)| (user.id, *kind) let:candidate>
        {
          let (user, kind) = candidate;
          let clicked = user.clone();
          let is_already_in = kind == Candidate::AlreadyIn;
          let is_checked = kind != Candidate::Pickable;
          view! {
            <li>
              <button
                type="button"
                role="checkbox"
                aria-checked=is_checked.to_string()
                aria-disabled=is_already_in.then_some("true")
                class="forward-item person-item"
                class:is-member=is_already_in
                class:is-picked=kind == Candidate::Picked
                prop:disabled=is_already_in
                on:click=move |_| toggle(clicked.clone())
              >
                <span class="pick-box" aria-hidden="true">{is_checked.then(|| view! { <Icon name=IconName::Check /> })}</span>
                <Avatar user=user.clone() size=AvatarSize::Small show_presence=true />
                <span translate="no">{user.display_name.clone()}</span>
                {(!user.username.is_empty()).then(|| view! { <span class="muted-text" translate="no">{format!("@{}", user.username)}</span> })}
                {user.is_guest.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
                {(user.device_setup == DeviceSetup::NoDevices).then(|| view! { <span class="muted-text">{move || i18n.t("directory-no-devices")}</span> })}
                {is_already_in.then(|| view! { <span class="muted-text person-note">{move || i18n.t(already_in_key)}</span> })}
              </button>
            </li>
          }
        }
      </For>
      <Show when=move || candidates.with(Vec::is_empty) fallback=|| ()>
        <li class="empty-state">{move || i18n.t("invite-nobody-found")}</li>
      </Show>
    </ul>
  }
}

/// The matches: not the user, no bots, guest accounts only where allowed, matching the filter; by
/// name, those already in (greyed out) last. Picked people stay in the list, checked.
fn candidates(
  mut users: Vec<User>,
  me: UserId,
  allows_guest_accounts: bool,
  picked: &HashSet<UserId>,
  already_in: &HashSet<UserId>,
  filter: &str,
) -> Vec<(User, Candidate)> {
  let needle = filter.trim().to_lowercase();
  users.retain(|user| {
    user.id != me
      && !user.is_bot
      && (allows_guest_accounts || !user.is_guest)
      && (needle.is_empty()
        || user.display_name.to_lowercase().contains(&needle)
        || user.username.to_lowercase().contains(&needle))
  });
  let mut list: Vec<(User, Candidate)> = users
    .into_iter()
    .map(|user| {
      let kind = if already_in.contains(&user.id) {
        Candidate::AlreadyIn
      } else if picked.contains(&user.id) {
        Candidate::Picked
      } else {
        Candidate::Pickable
      };
      (user, kind)
    })
    .collect();
  list.sort_by_key(|(user, kind)| (*kind == Candidate::AlreadyIn, user.display_name.to_lowercase()));
  list
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{PresenceStatus, id_from_raw};

  const ME: u64 = 1;

  #[test]
  fn picked_people_stay_checked_and_members_come_last_greyed_out() {
    let users = vec![
      person(ME, "Me"),
      person(2, "Bea"),
      person(3, "Al"),
      person(4, "Cy"),
    ];
    let picked = HashSet::from([id_from_raw(2)]);
    let already_in = HashSet::from([id_from_raw(3)]);
    let found = candidates(users, id_from_raw(ME), true, &picked, &already_in, "");
    let listed: Vec<(&str, Candidate)> = found
      .iter()
      .map(|(user, kind)| (user.display_name.as_str(), *kind))
      .collect();
    assert_eq!(
      listed,
      [
        ("Bea", Candidate::Picked),
        ("Cy", Candidate::Pickable),
        ("Al", Candidate::AlreadyIn)
      ]
    );
  }

  #[test]
  fn guest_accounts_and_bots_are_left_out_where_not_allowed() {
    let guest = User {
      is_guest: true,
      ..person(2, "Guest")
    };
    let bot = User {
      is_bot: true,
      ..person(3, "Bot")
    };
    let users = vec![guest, bot, person(4, "Member")];
    let names = |allows_guests| {
      candidates(
        users.clone(),
        id_from_raw(ME),
        allows_guests,
        &HashSet::new(),
        &HashSet::new(),
        "",
      )
      .into_iter()
      .map(|(user, _)| user.display_name)
      .collect::<Vec<_>>()
    };
    assert_eq!(names(false), ["Member"]);
    assert_eq!(names(true), ["Guest", "Member"]);
  }

  #[test]
  fn the_filter_matches_name_or_username() {
    let users = vec![person(2, "Bea Smith"), person(3, "Al Jones")];
    let found = candidates(
      users,
      id_from_raw(ME),
      true,
      &HashSet::new(),
      &HashSet::new(),
      "JONES",
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.display_name, "Al Jones");
  }

  fn person(id: u64, name: &str) -> User {
    User {
      id: id_from_raw(id),
      username: name.to_lowercase(),
      display_name: name.into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      device_setup: DeviceSetup::Unknown,
      maybe_email: None,
    }
  }
}
