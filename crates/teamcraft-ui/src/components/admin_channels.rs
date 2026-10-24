use leptos::prelude::*;

use teamcraft_proto::{ChannelModeration, HistoryPolicy, Permission};

use crate::admin::{AdminChannel, SelfJoinOutcome, format_bytes};
use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::channel_view::UnconfirmedRemovalActions;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind, UserId};
use crate::state::use_app_state;

const INACTIVE_AFTER_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelFilter {
  All,
  Private,
  WithGuests,
  Inactive,
  Orphaned,
}

impl ChannelFilter {
  const ALL: [ChannelFilter; 5] = [
    Self::All,
    Self::Private,
    Self::WithGuests,
    Self::Inactive,
    Self::Orphaned,
  ];

  fn slug(self) -> &'static str {
    match self {
      Self::All => "all",
      Self::Private => "private",
      Self::WithGuests => "guests",
      Self::Inactive => "inactive",
      Self::Orphaned => "orphaned",
    }
  }

  fn matches(self, channel: &AdminChannel, now: i64) -> bool {
    match self {
      Self::All => true,
      Self::Private => channel.kind == ConversationKind::Private,
      Self::WithGuests => channel.guest_count > 0,
      Self::Inactive => now - channel.last_activity > INACTIVE_AFTER_DAYS * DAY_MS,
      Self::Orphaned => channel.maybe_owner.is_none(),
    }
  }
}

/// This admin's removals from channels it isn't in that can't be confirmed (e2ee.md §4.6): the
/// admin removes again or keeps the person; the client never settles it on the server's word.
#[component]
fn UnconfirmedRemovals(channels: Memo<Vec<AdminChannel>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let removals = Memo::new(move |_| state.read(|backend| backend.unconfirmed_removals()));
  move || {
    removals
      .get()
      .into_iter()
      .map(|(conversation, user)| {
        let channel = channels
          .get()
          .into_iter()
          .find(|channel| channel.id == conversation)
          .map(|channel| channel.name)
          .unwrap_or_default();
        let name = state
          .read(|backend| backend.user(user))
          .map(|user| user.display_name)
          .filter(|name| !name.is_empty())
          .unwrap_or_else(|| i18n.t("left-in-group-unknown"));
        let text = i18n.t_with("removal-unconfirmed", &[("name", &name), ("channel", &channel)]);
        view! {
          <div class="key-banner" role="alert">
            <Icon name=IconName::Lock />
            <div class="key-banner-text">
              <strong translate="no">{text}</strong>
            </div>
            <UnconfirmedRemovalActions conversation=conversation user=user />
          </div>
        }
      })
      .collect_view()
  }
}

/// All spaces and channels including private ones, with metadata only.
#[component]
pub fn ChannelsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let filter = RwSignal::new(ChannelFilter::All);
  let selected = RwSignal::new(None::<ConversationId>);
  let now = now_ms();

  let channels = Memo::new(move |_| state.read(|backend| backend.admin_channels()));
  let visible = Memo::new(move |_| {
    let needle = query.get().trim().to_lowercase();
    let active = filter.get();
    channels
      .get()
      .into_iter()
      .filter(|channel| active.matches(channel, now))
      .filter(|channel| {
        needle.is_empty()
          || channel.name.to_lowercase().contains(&needle)
          || channel.space_name.to_lowercase().contains(&needle)
      })
      .collect::<Vec<_>>()
  });
  let selected_channel = Memo::new(move |_| {
    selected
      .get()
      .and_then(|id| channels.get().into_iter().find(|channel| channel.id == id))
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-channels-title"))
      subtitle=Signal::derive(move || i18n.t("admin-channels-subtitle"))
    />
    <UnconfirmedRemovals channels=channels />
    <div class="admin-toolbar">
      <label class="visually-hidden" for="admin-channel-search">{move || i18n.t("admin-search")}</label>
      <input
        id="admin-channel-search"
        class="input"
        type="search"
        name="q"
        autocomplete="off"
        spellcheck="false"
        placeholder=move || i18n.t("admin-search-placeholder")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
      <div class="segmented" role="group" aria-label=move || i18n.t("admin-filter")>
        {ChannelFilter::ALL
          .into_iter()
          .map(|option| {
            view! {
              <button aria-pressed=move || (filter.get() == option).to_string() on:click=move |_| filter.set(option)>
                {move || i18n.t(&format!("admin-filter-{}", option.slug()))}
              </button>
            }
          })
          .collect_view()}
      </div>
    </div>
    <div class="admin-split">
      <div class="table-wrap">
        <table class="data-table">
          <caption class="visually-hidden">{move || i18n.t("admin-channels-title")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("admin-col-channel")}</th>
              <th scope="col">{move || i18n.t("admin-col-space")}</th>
              <th scope="col" class="num">{move || i18n.t("admin-col-members")}</th>
              <th scope="col">{move || i18n.t("admin-col-owner")}</th>
              <th scope="col">{move || i18n.t("admin-col-activity")}</th>
              <th scope="col" class="num">{move || i18n.t("admin-col-storage")}</th>
            </tr>
          </thead>
          <tbody>
            <For each=move || visible.get() key=|channel| channel.id let:channel>
              <ChannelRow channel=channel selected=selected now=now />
            </For>
          </tbody>
        </table>
        <Show when=move || visible.get().is_empty() fallback=|| ()>
          <p class="empty-state">{move || i18n.t("admin-empty")}</p>
        </Show>
      </div>
      <aside class="detail-pane" aria-label=move || i18n.t("admin-detail-label")>
        {move || match selected_channel.get() {
          Some(channel) => view! { <ChannelDetail channel=channel /> }.into_any(),
          None => view! { <p class="empty-state">{move || i18n.t("admin-channel-select")}</p> }.into_any(),
        }}
      </aside>
    </div>
  }
}

#[component]
fn ChannelRow(channel: AdminChannel, selected: RwSignal<Option<ConversationId>>, now: i64) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.id;
  let owner = channel
    .maybe_owner
    .and_then(|owner| state.read(|backend| backend.user(owner)))
    .map(|user| user.display_name);
  let is_private = channel.kind == ConversationKind::Private;
  let icon = match channel.kind {
    ConversationKind::Private => IconName::Lock,
    ConversationKind::Integration => IconName::Terminal,
    _ => IconName::Hash,
  };
  let activity = move || format_relative(channel.last_activity, now, i18n.locale().tag());
  let is_selected = move || selected.get() == Some(id);

  view! {
    <tr class:selected=is_selected>
      <th scope="row">
        <button class="row-button" aria-pressed=move || is_selected().to_string() on:click=move |_| selected.set(Some(id))>
          <Icon name=icon />
          <span class="row-name" translate="no">{channel.name.clone()}</span>
          {is_private.then(|| view! { <span class="badge">{move || i18n.t("admin-kind-private")}</span> })}
          {channel.maybe_owner.is_none().then(|| view! { <span class="badge badge-warn">{move || i18n.t("admin-orphaned")}</span> })}
        </button>
      </th>
      <td translate="no">{channel.space_name.clone()}</td>
      <td class="num">
        {channel.member_count}
        {(channel.guest_count > 0).then(|| view! { <span class="muted-text">{format!(" (+{})", channel.guest_count)}</span> })}
      </td>
      <td translate="no">{owner.unwrap_or_else(|| "–".to_owned())}</td>
      <td>{activity}</td>
      <td class="num">{format_bytes(channel.storage_bytes)}</td>
    </tr>
  }
}

#[component]
fn ChannelDetail(channel: AdminChannel) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let outcome = RwSignal::new(None::<SelfJoinOutcome>);
  let id = channel.id;
  let is_private = channel.kind == ConversationKind::Private;
  let owner_name = channel
    .maybe_owner
    .and_then(|owner| state.read(|backend| backend.user(owner)))
    .map(|user| user.display_name);
  let retention = move || {
    let mut parts = Vec::new();
    if let Some(days) = channel.retention.maybe_days {
      parts.push(i18n.t_count("admin-retention-days", days as usize));
    }
    if let Some(messages) = channel.retention.maybe_max_messages {
      parts.push(i18n.t_count("admin-retention-messages", messages as usize));
    }
    if parts.is_empty() {
      i18n.t("admin-retention-forever")
    } else {
      parts.join(" / ")
    }
  };
  let history = match &channel.history {
    HistoryPolicy::FromJoin => "from_join".to_owned(),
    HistoryPolicy::Full => "full".to_owned(),
    HistoryPolicy::Unknown(other) => other.clone(),
  };

  view! {
    <h3 translate="no">{format!("# {}", channel.name)}</h3>
    <p class="muted-text" translate="no">{channel.space_name.clone()}</p>
    <dl class="meta-list">
      <dt>{move || i18n.t("admin-col-members")}</dt>
      <dd>{move || i18n.t_count("members-count", channel.member_count as usize)}</dd>
      <dt>{move || i18n.t("admin-guests")}</dt>
      <dd>{channel.guest_count}</dd>
      <dt>{move || i18n.t("admin-col-owner")}</dt>
      <dd translate="no">{owner_name.unwrap_or_else(|| "–".to_owned())}</dd>
      <dt>{move || i18n.t("admin-messages")}</dt>
      <dd>{channel.message_count.to_string()}</dd>
      <dt>{move || i18n.t("admin-col-storage")}</dt>
      <dd>{format_bytes(channel.storage_bytes)}</dd>
      <dt>{move || i18n.t("admin-history")}</dt>
      <dd>{move || {
        let key = format!("admin-history-{history}");
        let text = i18n.t(&key);
        if text == key { history.clone() } else { text }
      }}</dd>
      <dt>{move || i18n.t("admin-retention")}</dt>
      <dd>{retention}</dd>
    </dl>
    <p class="no-content-note">
      <Icon name=IconName::EyeOff />
      <span>{move || i18n.t("admin-no-content")}</span>
    </p>
    <div class="detail-actions">
      {is_private.then(|| {
        view! {
          <button class="btn" on:click=move |_| outcome.set(Some(state.write(|backend| backend.admin_join_channel(id))))>
            {move || i18n.t("admin-join")}
          </button>
        }
      })}
      <button class="btn" disabled title=move || i18n.t("admin-soon")>{move || i18n.t("admin-archive")}</button>
    </div>
    {channel.maybe_owner.is_none().then(|| view! { <OwnerAssign channel=id /> })}
    <ModerationEditor channel=id />
    <p class="status-line" role="status">
      {move || {
        outcome
          .get()
          .map(|result| match result {
            SelfJoinOutcome::Joined => i18n.t("admin-join-done"),
            SelfJoinOutcome::ApprovalRequested => i18n.t("admin-join-approval"),
            SelfJoinOutcome::Disabled => i18n.t("admin-join-disabled"),
          })
      }}
    </p>
  }
}

/// Picks a new owner for a channel without one, from its members (admin.md §2.5).
#[component]
fn OwnerAssign(channel: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let candidates = Memo::new(move |_| state.read(|backend| backend.channel_member_candidates(channel)));
  let chosen = RwSignal::new(None::<UserId>);
  let assigned = RwSignal::new(false);

  view! {
    <section class="owner-assign" aria-labelledby="owner-assign-title">
      <h4 id="owner-assign-title" class="detail-heading">{move || i18n.t("admin-assign-owner")}</h4>
      <p class="muted-text small">{move || i18n.t("admin-assign-owner-hint")}</p>
      <div class="field inline">
        <label class="visually-hidden" for="owner-candidate">{move || i18n.t("admin-owner-candidate")}</label>
        <select
          id="owner-candidate"
          class="input"
          on:change=move |ev| {
            let value = event_target_value(&ev);
            chosen.set(candidates.get().into_iter().find(|user| user.to_string() == value));
          }
        >
          <option value="">{move || i18n.t("admin-owner-choose")}</option>
          <For each=move || candidates.get() key=|user: &UserId| *user let:user>
            {
              let name = state.read(|backend| backend.user(user)).map(|found| found.display_name).unwrap_or_default();
              view! { <option value=user.to_string() translate="no">{name}</option> }
            }
          </For>
        </select>
        <button
          class="btn btn-primary"
          disabled=move || chosen.get().is_none()
          on:click=move |_| assign_owner(state, channel, chosen.get_untracked(), assigned)
        >
          {move || i18n.t("admin-assign")}
        </button>
      </div>
      <p class="status-line" role="status">{move || assigned.get().then(|| i18n.t("admin-owner-assigned"))}</p>
    </section>
  }
}

fn assign_owner(
  state: crate::state::AppState,
  channel: ConversationId,
  maybe_user: Option<UserId>,
  done: RwSignal<bool>,
) {
  if let Some(user) = maybe_user {
    state.write(|backend| backend.assign_channel_owner(channel, user));
    done.set(true);
  }
}

/// What members and guests may not do in this channel. A channel can only take permissions away.
#[component]
fn ModerationEditor(channel: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let moderation = Memo::new(move |_| state.read(|backend| backend.channel_moderation(channel)));

  view! {
    <section class="moderation" aria-labelledby="moderation-title">
      <h4 id="moderation-title" class="detail-heading">{move || i18n.t("admin-moderation")}</h4>
      <p class="muted-text small">{move || i18n.t("admin-moderation-hint")}</p>
      <table class="data-table moderation-table">
        <caption class="visually-hidden">{move || i18n.t("admin-moderation")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("permissions-permission")}</th>
            <th scope="col">{move || i18n.t("permissions-role-member")}</th>
            <th scope="col">{move || i18n.t("permissions-role-guest")}</th>
          </tr>
        </thead>
        <tbody>
          {MODERATABLE
            .into_iter()
            .map(|permission| {
              let key = format!("permission-{}", permission.as_str());
              let (member_perm, guest_perm) = (permission.clone(), permission.clone());
              let (member_set, guest_set) = (permission.clone(), permission);
              view! {
                <tr>
                  <th scope="row">{move || i18n.t(&key)}</th>
                  <td>
                    <label class="checkbox">
                      <input
                        type="checkbox"
                        prop:checked=move || !moderation.get().member_denied.contains(&member_perm)
                        on:change=move |ev| set_allowed(state, channel, moderation.get_untracked(), Who::Members, member_set.clone(), event_target_checked(&ev))
                      />
                      <span class="visually-hidden">{move || i18n.t("admin-moderation-allowed")}</span>
                    </label>
                  </td>
                  <td>
                    <label class="checkbox">
                      <input
                        type="checkbox"
                        prop:checked=move || !moderation.get().guest_denied.contains(&guest_perm)
                        on:change=move |ev| set_allowed(state, channel, moderation.get_untracked(), Who::Guests, guest_set.clone(), event_target_checked(&ev))
                      />
                      <span class="visually-hidden">{move || i18n.t("admin-moderation-allowed")}</span>
                    </label>
                  </td>
                </tr>
              }
            })
            .collect_view()}
        </tbody>
      </table>
    </section>
  }
}

/// The permissions a channel may take away (`Permission::is_channel_moderatable`).
const MODERATABLE: [Permission; 6] = [
  Permission::CreatePost,
  Permission::AddReaction,
  Permission::UseChannelMentions,
  Permission::UseGroupMentions,
  Permission::ManageBookmarks,
  Permission::ManageMembers,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
  Members,
  Guests,
}

fn set_allowed(
  state: crate::state::AppState,
  channel: ConversationId,
  current: ChannelModeration,
  who: Who,
  permission: Permission,
  is_allowed: bool,
) {
  let next = with_allowed(current, who, permission, is_allowed);
  state.write(|backend| backend.set_channel_moderation(channel, next));
}

fn with_allowed(
  mut moderation: ChannelModeration,
  who: Who,
  permission: Permission,
  is_allowed: bool,
) -> ChannelModeration {
  let denied = match who {
    Who::Members => &mut moderation.member_denied,
    Who::Guests => &mut moderation.guest_denied,
  };
  if is_allowed {
    denied.remove(&permission);
  } else {
    denied.insert(permission);
  }
  moderation
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn toggles_add_and_remove_denials_per_audience() {
    let denied = with_allowed(
      ChannelModeration::default(),
      Who::Guests,
      Permission::CreatePost,
      false,
    );
    assert!(denied.guest_denied.contains(&Permission::CreatePost));
    assert!(denied.member_denied.is_empty());
    let allowed = with_allowed(denied, Who::Guests, Permission::CreatePost, true);
    assert!(allowed.guest_denied.is_empty());
  }

  #[test]
  fn only_moderatable_permissions_are_offered() {
    assert!(MODERATABLE.iter().all(Permission::is_channel_moderatable));
  }
}
