//! The people directory (everyone in the workspace, with profile facts) and user groups.

use leptos::prelude::*;
use teamcraft_proto::{
  CreateUserGroupRequest, ListUserGroupsResponse, Role, UserGroupDetailResponse, UserGroupId,
  UserGroupResponse,
};

use crate::auth::ApiMethod;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::skeleton::ListSkeleton;
use crate::i18n::use_i18n;
use crate::model::{DeviceSetup, User, UserId};
use crate::profiles::use_profiles;
use crate::reactive::use_debounced;
use crate::rest::RestClient;
use crate::session::use_session;
use crate::state::{Overlay, use_app_state};

/// People shown at once; the filter narrows a larger workspace down.
const MAX_ROWS: usize = 200;
/// Placeholder rows while the user groups load.
const GROUP_SKELETON_ROWS: usize = 3;
/// The directory on the server is asked once typing paused this long.
const DIRECTORY_DEBOUNCE_MS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryTab {
  People,
  Groups,
}

#[component]
pub fn DirectoryView() -> impl IntoView {
  let i18n = use_i18n();
  let tab = RwSignal::new(DirectoryTab::People);
  view! {
    <section class="directory" aria-label=move || i18n.t("directory-title")>
      <header class="panel-header">
        <div class="panel-title"><h1>{move || i18n.t("directory-title")}</h1></div>
        <div class="tabs" role="tablist">
          <button role="tab" aria-selected=move || (tab.get() == DirectoryTab::People).to_string() on:click=move |_| tab.set(DirectoryTab::People)>
            {move || i18n.t("directory-people")}
          </button>
          <button role="tab" aria-selected=move || (tab.get() == DirectoryTab::Groups).to_string() on:click=move |_| tab.set(DirectoryTab::Groups)>
            {move || i18n.t("directory-groups")}
          </button>
        </div>
      </header>
      <div class="panel-scroll directory-body">
        {move || match tab.get() {
          DirectoryTab::People => view! { <PeopleList /> }.into_any(),
          DirectoryTab::Groups => view! { <UserGroups /> }.into_any(),
        }}
      </div>
    </section>
  }
}

#[component]
fn PeopleList() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let directory_query = use_debounced(filter, DIRECTORY_DEBOUNCE_MS);
  // Empty: everybody; otherwise only people of this organization.
  let organization = RwSignal::new(String::new());
  let organizations = Memo::new(move |_| {
    let mut names: Vec<String> = state
      .read(|backend| backend.users())
      .into_iter()
      .filter(|user| !user.is_bot)
      .filter_map(|user| user.maybe_organization)
      .collect();
    names.sort();
    names.dedup();
    names
  });
  let people = Memo::new(move |_| {
    let needle = filter.get().trim().to_lowercase();
    let selected = organization.get();
    let query = directory_query.get();
    // Who the app knows and the server's directory for the text (one page of it, never everybody):
    // colleagues who share no space or conversation with the user yet are found too.
    let mut all: Vec<User> = state.read(|backend| backend.search_people(query.trim()));
    all.retain(|user| !user.is_bot);
    if !selected.is_empty() {
      all.retain(|user| user.maybe_organization.as_deref() == Some(selected.as_str()));
    }
    all.sort_by_key(|user| user.display_name.to_lowercase());
    let hub = use_profiles();
    all
      .into_iter()
      .filter(|user| {
        if needle.is_empty() {
          return true;
        }
        let facts = hub
          .and_then(|hub| hub.profile(user.id))
          .map(|profile| {
            format!(
              "{} {}",
              profile.maybe_title.unwrap_or_default(),
              profile.maybe_department.unwrap_or_default()
            )
          })
          .unwrap_or_default();
        format!("{} {} {facts}", user.display_name, user.username)
          .to_lowercase()
          .contains(&needle)
      })
      .take(MAX_ROWS)
      .collect::<Vec<_>>()
  });
  view! {
    <label class="visually-hidden" for="directory-filter">{move || i18n.t("directory-filter")}</label>
    <input id="directory-filter" class="input" placeholder=move || i18n.t("directory-filter") prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev)) />
    <Show when=move || !organizations.get().is_empty() fallback=|| ()>
      <label class="visually-hidden" for="directory-organization">{move || i18n.t("directory-organization")}</label>
      <select id="directory-organization" class="input" prop:value=move || organization.get() on:change=move |ev| organization.set(event_target_value(&ev))>
        <option value="">{move || i18n.t("directory-all-organizations")}</option>
        <For each=move || organizations.get() key=|name| name.clone() let:name>
          <option value=name.clone() translate="no">{name.clone()}</option>
        </For>
      </select>
    </Show>
    <ul class="person-list">
      <For each=move || people.get() key=|user| (user.id, user.presence) let:user>
        <PersonRow user=user />
      </For>
      <Show when=move || people.get().is_empty() fallback=|| ()>
        <li class="empty-state">{move || i18n.t("directory-empty")}</li>
      </Show>
    </ul>
  }
}

#[component]
fn PersonRow(user: User) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = user.id;
  let facts = move || {
    use_profiles().and_then(|hub| hub.profile(id)).map(|profile| {
      [profile.maybe_title, profile.maybe_department]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    })
  };
  let is_me = id == state.me().id;
  let no_devices = user.device_setup == DeviceSetup::NoDevices;
  view! {
    <li class="person-row">
      <button class="person-main" on:click=move |_| state.overlay.set(Overlay::UserProfile(id))>
        <Avatar user=user.clone() size=AvatarSize::Medium show_presence=true />
        <span class="person-text">
          <span class="person-name" translate="no">{user.display_name.clone()}</span>
          <span class="person-facts" translate="no">{facts}</span>
          {no_devices.then(|| view! { <span class="person-facts">{move || i18n.t("directory-no-devices")}</span> })}
        </span>
        <crate::components::profile_views::InMeetingBadge user=id />
        {user.is_guest.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
        <crate::components::org_badge::OrgBadge organization=user.maybe_organization.clone() />
      </button>
      {(!is_me).then(|| view! { <button class="btn btn-small" on:click=move |_| state.open_direct(id)>{move || i18n.t("profile-message")}</button> })}
    </li>
  }
}

/// User groups: mentionable handles with members. Admins create and edit them.
#[component]
fn UserGroups() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let Some(session) = use_session() else {
    return view! { <p class="empty-state">{move || i18n.t("groups-unavailable")}</p> }.into_any();
  };
  let client = StoredValue::new_local(RestClient::new(session.authenticator()));
  let reload = RwSignal::new(0_u32);
  let groups = RwSignal::new(Vec::<UserGroupResponse>::new());
  let open = RwSignal::new(None::<UserGroupId>);
  let error = RwSignal::new(None::<String>);
  // Until the first answer the list shows placeholders, not "no groups".
  let is_loaded = RwSignal::new(false);
  let is_admin = Memo::new(move |_| state.read(|backend| backend.my_role()).includes(&Role::Admin));

  Effect::new(move |_| {
    reload.track();
    leptos::task::spawn_local(async move {
      match client
        .get_value()
        .get::<ListUserGroupsResponse>("/api/v1/user-groups")
        .await
      {
        Ok(list) => groups.set(list.groups),
        Err(failure) => error.set(Some(failure.to_string())),
      }
      is_loaded.set(true);
    });
  });

  let handle = RwSignal::new(String::new());
  let name = RwSignal::new(String::new());
  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let request = CreateUserGroupRequest {
      handle: handle
        .get_untracked()
        .trim()
        .trim_start_matches('@')
        .to_lowercase(),
      name: name.get_untracked().trim().to_owned(),
      maybe_description: None,
    };
    leptos::task::spawn_local(async move {
      match client
        .get_value()
        .post::<UserGroupResponse>("/api/v1/user-groups", &request)
        .await
      {
        Ok(_) => {
          handle.set(String::new());
          name.set(String::new());
          error.set(None);
          reload.update(|value| *value += 1);
        },
        Err(failure) => error.set(Some(failure.to_string())),
      }
    });
  };

  view! {
    {move || error.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
    <ul class="group-list">
      <For each=move || groups.get() key=|group| (group.id, group.member_count, group.name.clone()) let:group>
        {
          let id = group.id;
          view! {
            <li class="group-row">
              <button class="person-main" aria-expanded=move || (open.get() == Some(id)).to_string() on:click=move |_| open.update(|current| *current = if *current == Some(id) { None } else { Some(id) })>
                <span class="person-text">
                  <span class="person-name" translate="no">{group.name.clone()} " (@" {group.handle.clone()} ")"</span>
                  <span class="person-facts">{move || i18n.t_count("groups-members", group.member_count as usize)}</span>
                </span>
                {group.is_synced.then(|| view! { <span class="badge">{move || i18n.t("groups-synced")}</span> })}
              </button>
              <Show when=move || open.get() == Some(id) fallback=|| ()>
                <GroupMembers group=id is_synced=group.is_synced can_edit=is_admin client=client reload=reload />
              </Show>
            </li>
          }
        }
      </For>
      <Show when=move || !is_loaded.get() fallback=|| ()>
        <ListSkeleton rows=GROUP_SKELETON_ROWS label=Signal::derive(move || i18n.t("groups-loading")) />
      </Show>
      <Show when=move || is_loaded.get() && groups.get().is_empty() fallback=|| ()>
        <li class="empty-state">{move || i18n.t("groups-empty")}</li>
      </Show>
    </ul>
    <Show when=move || is_admin.get() fallback=|| ()>
      <form class="inline-form" on:submit=create>
        <label class="visually-hidden" for="group-handle">{move || i18n.t("groups-handle")}</label>
        <input id="group-handle" class="input" placeholder=move || i18n.t("groups-handle") required pattern="[a-z0-9_-]+" prop:value=move || handle.get() on:input=move |ev| handle.set(event_target_value(&ev)) />
        <label class="visually-hidden" for="group-name">{move || i18n.t("groups-name")}</label>
        <input id="group-name" class="input" placeholder=move || i18n.t("groups-name") required prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
        <button type="submit" class="btn btn-primary">{move || i18n.t("create")}</button>
      </form>
    </Show>
  }
  .into_any()
}

#[component]
fn GroupMembers(
  group: UserGroupId,
  is_synced: bool,
  can_edit: Memo<bool>,
  client: StoredValue<RestClient, LocalStorage>,
  reload: RwSignal<u32>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let members = RwSignal::new(Vec::<teamcraft_proto::UserSummary>::new());
  let refresh = RwSignal::new(0_u32);
  let chosen = RwSignal::new(String::new());
  Effect::new(move |_| {
    refresh.track();
    leptos::task::spawn_local(async move {
      let path = format!("/api/v1/user-groups/{group}");
      if let Ok(detail) = client.get_value().get::<UserGroupDetailResponse>(&path).await {
        members.set(detail.members);
      }
    });
  });
  let change = move |method: ApiMethod, user: UserId| {
    leptos::task::spawn_local(async move {
      let path = format!("/api/v1/user-groups/{group}/members/{user}");
      if client.get_value().send(method, &path, None).await.is_ok() {
        refresh.update(|value| *value += 1);
        reload.update(|value| *value += 1);
      }
    });
  };
  let candidates = Memo::new(move |_| {
    let present = members.get();
    state
      .read(|backend| backend.users())
      .into_iter()
      .filter(|user| !user.is_bot && !present.iter().any(|member| member.id == user.id))
      .collect::<Vec<_>>()
  });
  let can_change = move || can_edit.get() && !is_synced;
  view! {
    <ul class="group-members">
      <For each=move || members.get() key=|member| member.id let:member>
        {
          let id = member.id;
          view! {
            <li>
              <span translate="no">{member.display_name.clone()}</span>
              <Show when=can_change fallback=|| ()>
                <button class="btn btn-small" on:click=move |_| change(ApiMethod::Delete, id)>{move || i18n.t("groups-remove")}</button>
              </Show>
            </li>
          }
        }
      </For>
    </ul>
    <Show when=can_change fallback=|| ()>
      <div class="inline-form">
        <label class="visually-hidden" for=format!("group-add-{group}")>{move || i18n.t("groups-add")}</label>
        <select id=format!("group-add-{group}") class="input" on:change=move |ev| chosen.set(event_target_value(&ev))>
          <option value="">{move || i18n.t("channel-pick-person")}</option>
          <For each=move || candidates.get() key=|user| user.id let:user>
            <option value=user.id.to_string() translate="no">{user.display_name.clone()}</option>
          </For>
        </select>
        <button
          class="btn"
          prop:disabled=move || chosen.get().is_empty()
          on:click=move |_| {
            if let Ok(user) = chosen.get_untracked().parse::<UserId>() {
              change(ApiMethod::Put, user);
              chosen.set(String::new());
            }
          }
        >
          {move || i18n.t("groups-add")}
        </button>
      </div>
    </Show>
  }
}
