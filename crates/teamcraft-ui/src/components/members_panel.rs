use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::org_badge::OrgBadge;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind, User};
use crate::reactive::{items_by_key, keyed_item};
use crate::state::{Overlay, SidePanel, use_app_state};

#[component]
pub fn MembersPanel(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let members = Memo::new(move |_| {
    state.read(|backend| {
      let ids = backend
        .conversation(conversation)
        .map(|conv| conv.member_ids)
        .unwrap_or_default();
      ids
        .into_iter()
        .filter_map(|id| backend.user(id))
        .collect::<Vec<_>>()
    })
  });
  let by_id = items_by_key(members.into(), |user: &User| user.id);

  // Channels add people here; direct messages start a new group instead.
  let is_channel = state
    .read(|backend| backend.conversation(conversation))
    .is_some_and(|conv| matches!(conv.kind, ConversationKind::Public | ConversationKind::Private));
  view! {
    <aside class="side-panel members-panel" aria-label=move || i18n.t("members-panel-label")>
      <header class="panel-header">
        <div class="panel-title">
          <h2>{move || i18n.t("members-title")}</h2>
          <span class="panel-subtitle">{move || i18n.t_count("members-count", members.get().len())}</span>
        </div>
        {is_channel.then(|| view! {
          <button
            class="btn btn-small"
            on:click=move |_| state.overlay.set(Overlay::AddToChannel(conversation))
          >
            <Icon name=IconName::Plus />
            {move || i18n.t("channel-add-people")}
          </button>
        })}
        <button
          class="icon-btn"
          aria-label=move || i18n.t("close")
          title=move || i18n.t("close")
          on:click=move |_| state.panel.set(SidePanel::Closed)
        >
          <Icon name=IconName::Close />
        </button>
      </header>
      <ul class="member-list panel-scroll">
        // Rows are built once per person and follow their presence and status in place (issue #687).
        <For each=move || members.get() key=|user| user.id let:user>
          {
            let id = user.id;
            let user = keyed_item(by_id, id, user);
            view! {
              <li class="member">
                <button
                  type="button"
                  class="member-person"
                  aria-label=move || user.with(|user| i18n.t_with("profile-open", &[("name", &user.display_name)]))
                  on:click=move |_| state.overlay.set(Overlay::UserProfile(id))
                >
                  {move || view! { <Avatar user=user.get() size=AvatarSize::Medium show_presence=true /> }}
                  <span class="member-name" translate="no">
                    {move || user.with(|user| user.display_name.clone())}
                  </span>
                </button>
                <crate::components::profile_views::InMeetingBadge user=id />
                {move || user.with(|user| user.is_guest).then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
                {move || view! { <OrgBadge organization=user.with(|user| user.maybe_organization.clone()) /> }}
                {move || {
                  user
                    .with(|user| user.maybe_custom_status.clone())
                    .map(|status| view! { <span class="member-status" title=status.text.clone()>{status.emoji}</span> })
                }}
              </li>
            }
          }
        </For>
      </ul>
    </aside>
  }
}
