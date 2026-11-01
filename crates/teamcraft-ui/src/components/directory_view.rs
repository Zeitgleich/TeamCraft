//! The people directory (everyone in the workspace, with profile facts) and user groups.

use leptos::prelude::*;
use teamcraft_proto::{
  CreateUserGroupRequest, ErrorCode, ListUserGroupsResponse, Permission, UserGroupDetailResponse,
  UserGroupId, UserGroupResponse,
};

use crate::auth::ApiMethod;
use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::channel_access::can_in_workspace_now;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::skeleton::ListSkeleton;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::tab_list::{on_tab_keys, roving_tabindex};
use crate::feedback::{FailureKind, report_failure};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::{DeviceSetup, User, UserId};
use crate::profiles::use_profiles;
use crate::reactive::{SetIfAlive, UpdateIfAlive, use_debounced};
use crate::rest::RestClient;
use crate::session::use_session;
use crate::state::{Overlay, use_app_state};

/// People shown at once; the filter narrows a larger workspace down.
const MAX_ROWS: usize = 200;
/// Placeholder rows while the user groups load.
const GROUP_SKELETON_ROWS: usize = 3;
/// The directory on the server is asked once typing paused this long.
const DIRECTORY_DEBOUNCE_MS: u64 = 250;

/// Element ID of the panel the directory tabs control.
const DIRECTORY_PANEL_ID: &str = "directory-panel";
const GROUP_FORM_ID: &str = "group-create";
const GROUP_HANDLE_ID: &str = "group-handle";
const GROUP_NAME_ID: &str = "group-name";
/// Server refusals of a new group that are about its handle: taken, or malformed or reserved.
const GROUP_SERVER_FIELDS: [(ErrorCode, &str); 2] = [
  (ErrorCode::Conflict, GROUP_HANDLE_ID),
  (ErrorCode::InvalidRequest, GROUP_HANDLE_ID),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryTab {
  People,
  Groups,
}

impl DirectoryTab {
  const ALL: [Self; 2] = [Self::People, Self::Groups];

  fn label_key(self) -> &'static str {
    match self {
      Self::People => "directory-people",
      Self::Groups => "directory-groups",
    }
  }

  fn tab_id(self) -> String {
    format!("{}-tab", self.label_key())
  }
}

#[component]
pub fn DirectoryView() -> impl IntoView {
  let i18n = use_i18n();
  let tab = RwSignal::new(DirectoryTab::People);
  let on_keydown =
    move |ev: web_sys::KeyboardEvent| on_tab_keys(&ev, &DirectoryTab::ALL, tab, DirectoryTab::tab_id);
  view! {
    <section class="directory page-view" aria-label=move || i18n.t("directory-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title"><h1>{move || i18n.t("directory-title")}</h1></div>
        <div class="segmented" role="tablist" aria-label=move || i18n.t("directory-title") on:keydown=on_keydown>
          <SlidingIndicator />
          {DirectoryTab::ALL
            .into_iter()
            .map(|value| {
              view! {
                <button
                  id=value.tab_id()
                  role="tab"
                  aria-selected=move || (tab.get() == value).to_string()
                  aria-controls=DIRECTORY_PANEL_ID
                  tabindex=move || roving_tabindex(tab.get() == value)
                  on:click=move |_| tab.set(value)
                >
                  {move || i18n.t(value.label_key())}
                </button>
              }
            })
            .collect_view()}
        </div>
      </header>
      <div class="panel-scroll directory-body" role="tabpanel" id=DIRECTORY_PANEL_ID aria-labelledby=move || tab.get().tab_id()>
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
    <div class="page-tools">
      <label class="page-search">
        <span class="visually-hidden">{move || i18n.t("directory-filter")}</span>
        <Icon name=IconName::Search />
        <input
          id="directory-filter"
          class="input"
          type="search"
          autocomplete="off"
          placeholder=move || i18n.t("directory-filter")
          prop:value=move || filter.get()
          on:input=move |ev| filter.set(event_target_value(&ev))
        />
      </label>
      <Show when=move || !organizations.get().is_empty() fallback=|| ()>
        <label class="visually-hidden" for="directory-organization">{move || i18n.t("directory-organization")}</label>
        <select id="directory-organization" class="input page-select" prop:value=move || organization.get() on:change=move |ev| organization.set(event_target_value(&ev))>
          <option value="">{move || i18n.t("directory-all-organizations")}</option>
          <For each=move || organizations.get() key=|name| name.clone() let:name>
            <option value=name.clone() translate="no">{name.clone()}</option>
          </For>
        </select>
      </Show>
    </div>
    <ul class="person-list page-rows">
      <For each=move || people.get() key=|user| (user.id, user.presence) let:user>
        <PersonRow user=user />
      </For>
      <Show when=move || people.get().is_empty() fallback=|| ()>
        <EmptyState icon=IconName::Users title_key="directory-empty-title" body_key="directory-empty" as_item=true />
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
  let direct = (!is_me).then(|| state.direct_with(&[id])).flatten();
  // Hidden, not failing after the click: a guest who may not start conversations is offered none.
  let can_message = crate::channel_access::can_start_direct(state, &[id]);
  view! {
    <li class="page-row person-row">
      <button class="page-row-main person-main" on:click=move |_| state.overlay.set(Overlay::UserProfile(id))>
        <Avatar user=user.clone() size=AvatarSize::Large show_presence=true />
        <span class="page-row-text person-text">
          <span class="page-row-title person-name" translate="no">
            {user.display_name.clone()}
            {is_me.then(|| view! { " " <span class="badge">{move || i18n.t("directory-you")}</span> })}
          </span>
          <span class="page-row-detail person-facts" translate="no">{facts}</span>
          {no_devices.then(|| view! { <span class="page-row-detail">{move || i18n.t("directory-no-devices")}</span> })}
        </span>
        <crate::components::profile_views::InMeetingBadge user=id />
        {(user.is_guest && state.shows_guest_markers()).then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
        <crate::components::org_badge::OrgBadge organization=user.maybe_organization.clone() />
      </button>
      {(!is_me).then(|| view! {
        <span class="page-row-actions">
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

/// A typed group handle as the server stores it: without a leading `@`, lower case.
fn normalized_handle(typed: &str) -> String {
  typed.trim().trim_start_matches('@').to_lowercase()
}

/// Whether a handle has only the characters the server accepts (it also checks the length and
/// reserved names).
fn is_group_handle(handle: &str) -> bool {
  !handle.is_empty()
    && handle
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
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
  // The server asks for the workspace permissions `create_user_group` and
  // `manage_user_group_members` (admins and owners in the built-in scheme).
  let can_create = Memo::new(move |_| can_in_workspace_now(state, &Permission::CreateUserGroup));
  let can_edit_members = Memo::new(move |_| can_in_workspace_now(state, &Permission::ManageUserGroupMembers));

  Effect::new(move |_| {
    reload.track();
    leptos::task::spawn_local(async move {
      match client
        .get_value()
        .get::<ListUserGroupsResponse>("/api/v1/user-groups")
        .await
      {
        Ok(list) => groups.set_if_alive(list.groups),
        Err(failure) => error.set_if_alive(Some(failure.to_string())),
      }
      is_loaded.set_if_alive(true);
    });
  });

  let handle = RwSignal::new(String::new());
  let name = RwSignal::new(String::new());
  let errors = FormErrors::new();
  let field_errors = move || {
    let mut found = FieldErrors::new();
    if !is_group_handle(&normalized_handle(&handle.get())) {
      found.push(GROUP_HANDLE_ID, i18n.t("groups-error-handle"));
    }
    if name.with(|name| name.trim().is_empty()) {
      found.push(GROUP_NAME_ID, i18n.t("form-error-name"));
    }
    found
  };
  Effect::new(move |_| errors.follow(field_errors()));
  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(GROUP_FORM_ID, found);
      return;
    }
    let request = CreateUserGroupRequest {
      handle: normalized_handle(&handle.get_untracked()),
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
          handle.set_if_alive(String::new());
          name.set_if_alive(String::new());
          error.set_if_alive(None);
          reload.update_if_alive(|value| *value += 1);
        },
        Err(refusal) => {
          let failure = refusal.to_failure();
          // A taken or reserved handle goes to the handle field; anything else is a toast.
          let message = match &failure.kind {
            FailureKind::Server(ErrorCode::Conflict) => i18n.t("groups-error-handle-taken"),
            _ => i18n.t("groups-error-handle"),
          };
          if !errors.refuse_server(GROUP_FORM_ID, &failure, &GROUP_SERVER_FIELDS, message) {
            report_failure(failure);
          }
        },
      }
    });
  };

  view! {
    {move || error.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
    <ul class="group-list page-rows">
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
                <GroupMembers group=id is_synced=group.is_synced can_edit=can_edit_members client=client reload=reload />
              </Show>
            </li>
          }
        }
      </For>
      <Show when=move || !is_loaded.get() fallback=|| ()>
        <ListSkeleton rows=GROUP_SKELETON_ROWS label=Signal::derive(move || i18n.t("groups-loading")) />
      </Show>
      <Show when=move || is_loaded.get() && groups.get().is_empty() fallback=|| ()>
        <EmptyState icon=IconName::Users title_key="groups-empty-title" body_key="groups-empty" as_item=true />
      </Show>
    </ul>
    <Show when=move || can_create.get() fallback=|| ()>
      <form id=GROUP_FORM_ID class="group-create" on:submit=create>
        <div class="inline-form">
          <div class="group-create-field">
            <label class="visually-hidden" for=GROUP_HANDLE_ID>{move || i18n.t("groups-handle")}</label>
            <input id=GROUP_HANDLE_ID class="input" placeholder=move || i18n.t("groups-handle") aria-required="true"
              aria-invalid=errors.invalid(GROUP_HANDLE_ID) aria-describedby=errors.described(GROUP_HANDLE_ID)
              prop:value=move || handle.get() on:input=move |ev| handle.set(event_target_value(&ev)) />
            <FieldError errors=errors field=GROUP_HANDLE_ID />
          </div>
          <div class="group-create-field">
            <label class="visually-hidden" for=GROUP_NAME_ID>{move || i18n.t("groups-name")}</label>
            <input id=GROUP_NAME_ID class="input" placeholder=move || i18n.t("groups-name") aria-required="true"
              aria-invalid=errors.invalid(GROUP_NAME_ID) aria-describedby=errors.described(GROUP_NAME_ID)
              prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
            <FieldError errors=errors field=GROUP_NAME_ID />
          </div>
          <button type="submit" class="btn btn-primary">{move || i18n.t("create")}</button>
        </div>
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
        members.set_if_alive(detail.members);
      }
    });
  });
  let change = move |method: ApiMethod, user: UserId| {
    leptos::task::spawn_local(async move {
      let path = format!("/api/v1/user-groups/{group}/members/{user}");
      if client.get_value().send(method, &path, None).await.is_ok() {
        refresh.update_if_alive(|value| *value += 1);
        reload.update_if_alive(|value| *value += 1);
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_typed_handle_is_read_like_the_server_reads_it() {
    assert_eq!(normalized_handle(" @Design-Team "), "design-team");
    assert!(is_group_handle("design_team-2"));
    assert!(!is_group_handle(""));
    assert!(!is_group_handle("design team"));
  }
}
