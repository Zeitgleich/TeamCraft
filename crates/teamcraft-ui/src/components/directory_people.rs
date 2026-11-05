//! The people grid of the directory (#298): cards with a large picture, name, title and presence,
//! narrowed by search and profile filters on the server, one page at a time. The People and External
//! tabs are this view for members and for guests.

use leptos::prelude::*;
use teamcraft_client_api::backend::Backend;
use teamcraft_client_api::list_load::{ListLoad, REQUEST_DEADLINE_MS};
use teamcraft_proto::{
  DirectoryFiltersResponse, DirectoryPeopleResponse, DirectoryPerson, UserKind, UserSummary,
};

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::channel_access::is_guest;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::directory_pager::{Pager, PagerFooter, query_path};
use crate::components::directory_view::InvitePeopleButton;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::list_status::{ListPlaceholder, ListStatus};
use crate::i18n::use_i18n;
use crate::live::presence_status;
use crate::model::{DeviceSetup, PresenceStatus, User, raw_id};
use crate::reactive::{SetIfAlive, use_debounced};
use crate::rest::RestClient;
use crate::session::use_session;
use crate::state::{Overlay, use_app_state};

/// Placeholder cards while the first page loads.
const SKELETON_ROWS: usize = 6;
/// The directory on the server is asked once typing paused this long.
const SEARCH_DEBOUNCE_MS: u64 = 250;
/// Above this the local fallback stops: it is a stopgap for when the server can't be reached.
const MAX_LOCAL_PEOPLE: usize = 200;

/// Who a people view lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeopleTab {
  People,
  External,
}

/// One card's data: the server's facts, and the live state of somebody this device follows.
#[derive(Clone, Debug, PartialEq)]
struct PersonCardData {
  user: User,
  maybe_title: Option<String>,
  maybe_department: Option<String>,
}

impl PeopleTab {
  fn kind(self, am_guest: bool) -> Option<UserKind> {
    match self {
      // A guest's own list is who they share a conversation with, guests among them.
      Self::People if am_guest => None,
      Self::People => Some(UserKind::Member),
      Self::External => Some(UserKind::Guest),
    }
  }
}

#[component]
pub fn PeopleGrid(tab: PeopleTab) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let text = RwSignal::new(String::new());
  let debounced = use_debounced(text, SEARCH_DEBOUNCE_MS);
  let organization = RwSignal::new(String::new());
  let department = RwSignal::new(String::new());
  let title = RwSignal::new(String::new());
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let am_guest = is_guest(state);
  let kind_value = tab
    .kind(am_guest)
    .map(|kind| kind.as_str().to_owned())
    .unwrap_or_default();
  let kind_param = kind_value.clone();
  let path = Signal::derive(move || {
    let space = scope.get().map(|space| space.to_string()).unwrap_or_default();
    query_path(
      "/api/v1/directory/people",
      &[
        ("query", debounced.get().trim()),
        ("kind", &kind_param),
        ("organization", &organization.get()),
        ("department", &department.get()),
        ("title", &title.get()),
        ("space_id", &space),
      ],
    )
  });
  let pager = Pager::<DirectoryPerson>::new::<DirectoryPeopleResponse>(path);
  // A guest has no filters and no roster of a space.
  let is_hidden_roster = move || am_guest && scope.get().is_some();

  let filters = RwSignal::new(DirectoryFiltersResponse::default());
  Effect::new(move |_| {
    let maybe_space = scope.get();
    // What was picked in one space may not exist in the next.
    organization.set(String::new());
    department.set(String::new());
    title.set(String::new());
    let Some(session) = use_session().filter(|_| !am_guest) else {
      return;
    };
    let space = maybe_space.map(|space| space.to_string()).unwrap_or_default();
    let filters_path = query_path(
      "/api/v1/directory/filters",
      &[("kind", &kind_value), ("space_id", &space)],
    );
    let client = RestClient::new(session.authenticator());
    leptos::task::spawn_local(async move {
      // The grid works without its filters; they come back with the next visit.
      if let Ok(values) = client.get::<DirectoryFiltersResponse>(&filters_path).await {
        filters.set_if_alive(values);
      }
    });
  });

  let has_failed = move || matches!(pager.load.get(), ListLoad::Failed { .. });
  // The server can't be asked: who this device knows, narrowed here, rather than nothing.
  let local_people = Memo::new(move |_| {
    if !has_failed() || tab == PeopleTab::External {
      return Vec::new();
    }
    let needle = text.get().trim().to_lowercase();
    state
      .read(|backend| backend.search_people(&needle))
      .into_iter()
      .filter(|user| !user.is_bot && !user.is_deactivated && (am_guest || !user.is_guest))
      .take(MAX_LOCAL_PEOPLE)
      .map(|user| PersonCardData {
        user,
        maybe_title: None,
        maybe_department: None,
      })
      .collect::<Vec<_>>()
  });
  let cards = Memo::new(move |_| {
    let rows = pager.rows.get();
    if rows.is_empty() {
      return local_people.get();
    }
    state.read(|backend| rows.iter().map(|person| card_data(backend, person)).collect())
  });
  let status = ListStatus::new(
    move || cards.with(Vec::is_empty),
    move || {
      if text.with(|typed| typed.trim() != debounced.get().trim()) {
        ListLoad::Loading
      } else {
        pager.load.get()
      }
    },
    Signal::derive(move || path.get()),
    REQUEST_DEADLINE_MS,
  );
  let on_retry = Callback::new(move |()| {
    status.restart();
    pager.reload();
  });
  let on_more = Callback::new(move |()| pager.load_more::<DirectoryPeopleResponse>());
  let empty_body_key = move || match tab {
    PeopleTab::External => "directory-external-empty",
    PeopleTab::People if scope.get().is_some() => "directory-space-empty",
    PeopleTab::People => "directory-empty",
  };
  let filter_id = match tab {
    PeopleTab::People => "directory-filter",
    PeopleTab::External => "directory-external-filter",
  };

  view! {
    <Show
      when=move || !is_hidden_roster()
      fallback=|| view! { <EmptyState icon=IconName::Users title_key="directory-empty-title" body_key="directory-space-hidden" /> }
    >
      <div class="page-tools directory-tools">
        <label class="page-search">
          <span class="visually-hidden">{move || i18n.t("directory-filter")}</span>
          <Icon name=IconName::Search />
          <input
            id=filter_id
            class="input"
            type="search"
            autocomplete="off"
            placeholder=move || i18n.t("directory-filter")
            prop:value=move || text.get()
            on:input=move |ev| text.set(event_target_value(&ev))
          />
        </label>
        <FilterSelect
          id="directory-organization"
          label_key="directory-organization"
          all_key="directory-all-organizations"
          selected=organization
          values=Signal::derive(move || filters.get().organizations)
        />
        <FilterSelect
          id="directory-department"
          label_key="directory-department"
          all_key="directory-all-departments"
          selected=department
          values=Signal::derive(move || filters.get().departments)
        />
        <FilterSelect
          id="directory-title"
          label_key="directory-job-title"
          all_key="directory-all-titles"
          selected=title
          values=Signal::derive(move || filters.get().titles)
        />
        <InvitePeopleButton />
      </div>
      <Show when=move || !local_people.with(Vec::is_empty) fallback=|| ()>
        <div class="directory-offline" role="status">
          <span>{move || i18n.t("directory-offline-note")}</span>
          <button type="button" class="btn btn-small" on:click=move |_| on_retry.run(())>{move || i18n.t("data-retry")}</button>
        </div>
      </Show>
      <ul class="people-grid">
        <For each=move || cards.get() key=|card| (card.user.id, card.user.presence) let:card>
          <PersonCard card=card />
        </For>
      </ul>
      <ul class="page-rows">
        <ListPlaceholder
          phase=status.phase
          rows=SKELETON_ROWS
          label=Signal::derive(move || i18n.t("directory-loading"))
          on_retry=on_retry
          as_item=true
        >
          {move || {
            let body_key = empty_body_key();
            view! { <EmptyState icon=IconName::Users title_key="directory-empty-title" body_key=body_key as_item=true /> }
          }}
        </ListPlaceholder>
      </ul>
      <PagerFooter pager=pager on_more=on_more />
    </Show>
  }
}

/// A profile filter. Only offered when there is something to choose from.
#[component]
fn FilterSelect(
  id: &'static str,
  label_key: &'static str,
  all_key: &'static str,
  selected: RwSignal<String>,
  values: Signal<Vec<String>>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <Show when=move || values.with(|values| !values.is_empty()) fallback=|| ()>
      <label class="visually-hidden" for=id>{move || i18n.t(label_key)}</label>
      <select id=id class="input page-select" prop:value=move || selected.get() on:change=move |ev| selected.set(event_target_value(&ev))>
        <option value="">{move || i18n.t(all_key)}</option>
        <For each=move || values.get() key=|value| value.clone() let:value>
          <option value=value.clone() translate="no">{value.clone()}</option>
        </For>
      </select>
    </Show>
  }
}

/// Somebody this device knows comes with their live presence; the server's answer names the rest.
fn card_data(backend: &dyn Backend, person: &DirectoryPerson) -> PersonCardData {
  let from_answer = user_of_summary(&person.user, presence_status(&person.presence));
  let user = backend
    .user(person.user.id)
    .map_or(from_answer.clone(), |known| User {
      device_setup: from_answer.device_setup,
      maybe_organization: from_answer.maybe_organization.clone(),
      ..known
    });
  PersonCardData {
    user,
    maybe_title: person.maybe_title.clone(),
    maybe_department: person.maybe_department.clone(),
  }
}

fn user_of_summary(summary: &UserSummary, presence: PresenceStatus) -> User {
  User {
    id: summary.id,
    username: summary.maybe_username.clone().unwrap_or_default(),
    display_name: summary.display_name.clone(),
    presence,
    maybe_custom_status: None,
    avatar_hue: (raw_id(summary.id) % 360) as u16,
    is_guest: summary.kind == UserKind::Guest,
    is_bot: summary.kind == UserKind::Bot,
    maybe_organization: summary.maybe_organization.clone(),
    device_setup: match summary.maybe_has_devices {
      Some(true) => DeviceSetup::HasDevices,
      Some(false) => DeviceSetup::NoDevices,
      None => DeviceSetup::Unknown,
    },
    is_deactivated: summary.is_deactivated,
    is_deleted: summary.is_deleted,
    maybe_email: summary.maybe_email.clone(),
  }
}

/// Title and department as one line.
fn facts_line(maybe_title: Option<&str>, maybe_department: Option<&str>) -> String {
  [maybe_title, maybe_department]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn presence_key(presence: PresenceStatus) -> &'static str {
  match presence {
    PresenceStatus::Online => "presence-online",
    PresenceStatus::Away => "presence-away",
    PresenceStatus::DoNotDisturb => "presence-dnd",
    PresenceStatus::Offline => "presence-offline",
  }
}

#[component]
fn PersonCard(card: PersonCardData) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let user = card.user;
  let id = user.id;
  let is_me = id == state.me().id;
  let no_devices = user.device_setup == DeviceSetup::NoDevices;
  let direct = (!is_me).then(|| state.direct_with(&[id])).flatten();
  // Hidden, not failing after the click: a guest who may not start conversations is offered none.
  let can_message = crate::channel_access::can_start_direct(state, &[id]);
  let facts = facts_line(card.maybe_title.as_deref(), card.maybe_department.as_deref());
  let presence = presence_key(user.presence);
  view! {
    <li class="person-card">
      <button class="person-card-main" on:click=move |_| state.overlay.set(Overlay::UserProfile(id))>
        <Avatar user=user.clone() size=AvatarSize::ExtraLarge show_presence=true />
        <span class="person-card-name" translate="no">
          {user.display_name.clone()}
          {is_me.then(|| view! { " " <span class="badge">{move || i18n.t("directory-you")}</span> })}
        </span>
        <span class="person-card-facts" translate="no">{facts}</span>
        <span class="person-card-presence">{move || i18n.t(presence)}</span>
        {no_devices.then(|| view! { <span class="person-card-facts">{move || i18n.t("directory-no-devices")}</span> })}
        <span class="person-card-badges">
          <crate::components::profile_views::InMeetingBadge user=id />
          {(user.is_guest && state.shows_guest_markers()).then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
          <crate::components::org_badge::OrgBadge organization=user.maybe_organization.clone() />
        </span>
      </button>
      {(!is_me && (can_message || direct.is_some())).then(|| view! {
        <span class="person-card-actions">
          {can_message.then(|| view! {
            <button class="icon-btn" aria-label=move || i18n.t("profile-message") title=move || i18n.t("profile-message") on:click=move |_| state.open_direct(id)>
              <Icon name=IconName::Chat />
            </button>
          })}
          {direct.map(|conversation| view! {
            <button
              class="icon-btn"
              aria-label=move || i18n.t("profile-call")
              title=move || i18n.t("profile-call")
              disabled=move || state.call.phase.get() != CallPhase::Idle || !state.read(|backend| backend.has_calls())
              on:click=move |_| state.call.prepare(CallRequest { conversation, kind: CallKind::Direct }, false)
            >
              <Icon name=IconName::Phone />
            </button>
          })}
        </span>
      })}
    </li>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_facts_line_leaves_out_what_is_missing() {
    assert_eq!(facts_line(Some("Lead"), Some("Sales")), "Lead · Sales");
    assert_eq!(facts_line(None, Some("Sales")), "Sales");
    assert_eq!(facts_line(None, None), "");
  }

  #[test]
  fn each_tab_asks_for_its_kind_of_people() {
    assert_eq!(PeopleTab::External.kind(false), Some(UserKind::Guest));
    assert_eq!(PeopleTab::People.kind(false), Some(UserKind::Member));
    assert_eq!(
      PeopleTab::People.kind(true),
      None,
      "a guest's list holds whoever they share with"
    );
  }

  #[test]
  fn a_summary_says_whether_the_person_has_a_device() {
    let mut summary = UserSummary {
      id: "7".parse().unwrap(),
      maybe_username: None,
      display_name: "Ann".to_owned(),
      kind: UserKind::Guest,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: Some(false),
      is_deactivated: false,
      is_deleted: false,
    };
    let user = user_of_summary(&summary, PresenceStatus::Away);
    assert_eq!(user.device_setup, DeviceSetup::NoDevices);
    assert!(user.is_guest);
    assert_eq!(user.presence, PresenceStatus::Away);
    summary.maybe_has_devices = None;
    assert_eq!(
      user_of_summary(&summary, PresenceStatus::Away).device_setup,
      DeviceSetup::Unknown
    );
  }
}
