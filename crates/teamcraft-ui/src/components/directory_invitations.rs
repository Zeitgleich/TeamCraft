//! The Invitations tab of the directory (#298): open invitations the user may manage, newest first,
//! with resend and revoke. The server decides what is listed and allowed (docs/admin.md); the
//! buttons follow its answer.

use leptos::prelude::*;
use teamcraft_client_api::list_load::{ListLoad, REQUEST_DEADLINE_MS};
use teamcraft_proto::{DirectoryInvitation, DirectoryInvitationsResponse, ErrorCode, InviteId, SpaceRole};

use crate::auth::ApiMethod;
use crate::clock::format_date;
use crate::components::directory_pager::{Pager, PagerFooter, query_path};
use crate::components::directory_view::InvitePeopleButton;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::list_status::{ListPlaceholder, ListStatus};
use crate::components::pending_button::PendingButton;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::reactive::{SetIfAlive, use_debounced};
use crate::rest::RestClient;
use crate::state::{Overlay, use_app_state};

const SKELETON_ROWS: usize = 4;
const SEARCH_DEBOUNCE_MS: u64 = 250;

#[component]
pub fn InvitationList() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let text = RwSignal::new(String::new());
  let debounced = use_debounced(text, SEARCH_DEBOUNCE_MS);
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let path = Signal::derive(move || {
    let space = scope.get().map(|space| space.to_string()).unwrap_or_default();
    query_path(
      "/api/v1/directory/invitations",
      &[("query", debounced.get().trim()), ("space_id", &space)],
    )
  });
  let pager = Pager::<DirectoryInvitation>::new::<DirectoryInvitationsResponse>(path);
  // Invitations sent from the dialog show once it closes.
  Effect::new(move |maybe_was_open: Option<bool>| {
    let is_open = state
      .overlay
      .with(|overlay| matches!(overlay, Overlay::InvitePeople(_)));
    if maybe_was_open == Some(true) && !is_open {
      pager.reload();
    }
    is_open
  });
  let status = ListStatus::new(
    move || pager.rows.with(Vec::is_empty),
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
  let on_more = Callback::new(move |()| pager.load_more::<DirectoryInvitationsResponse>());
  view! {
    <div class="page-tools directory-tools">
      <label class="page-search">
        <span class="visually-hidden">{move || i18n.t("directory-invitation-filter")}</span>
        <Icon name=IconName::Search />
        <input
          id="directory-invitation-filter"
          class="input"
          type="search"
          autocomplete="off"
          placeholder=move || i18n.t("directory-invitation-filter")
          prop:value=move || text.get()
          on:input=move |ev| text.set(event_target_value(&ev))
        />
      </label>
      <InvitePeopleButton />
    </div>
    <ul class="page-rows">
      <For each=move || pager.rows.get() key=|invitation| invitation.id let:invitation>
        <InvitationRow invitation=invitation pager=pager />
      </For>
      <ListPlaceholder
        phase=status.phase
        rows=SKELETON_ROWS
        label=Signal::derive(move || i18n.t("directory-invitations-loading"))
        on_retry=on_retry
        as_item=true
      >
        <EmptyState icon=IconName::UserPlus title_key="directory-invitations-empty-title" body_key="directory-invitations-empty" as_item=true />
      </ListPlaceholder>
    </ul>
    <PagerFooter pager=pager on_more=on_more />
  }
}

#[component]
fn InvitationRow(invitation: DirectoryInvitation, pager: Pager<DirectoryInvitation>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = invitation.id;
  let space = invitation.space_id;
  let resend = PendingAction::new();
  let revoke = PendingAction::new();
  let is_resent = RwSignal::new(false);
  let is_confirming = RwSignal::new(false);
  let on_resend = move |_| {
    let maybe_client = pager.client();
    resend.spawn(async move {
      let path = format!("/api/v1/spaces/{space}/invites/{id}/resend");
      match call(maybe_client, ApiMethod::Post, path).await {
        Ok(()) => {
          is_resent.set_if_alive(true);
          Ok(())
        },
        Err(failure) => {
          // Gone meanwhile (accepted, or revoked elsewhere): the row has nothing left to offer.
          if is_gone(&failure) {
            forget(pager, id);
          }
          Err(failure)
        },
      }
    });
  };
  let on_revoke = move |_| {
    let maybe_client = pager.client();
    revoke.spawn(async move {
      let path = format!("/api/v1/spaces/{space}/invites/{id}");
      match call(maybe_client, ApiMethod::Delete, path).await {
        Ok(()) => {},
        // Already gone is what was asked for.
        Err(failure) if is_gone(&failure) => {},
        Err(failure) => return Err(failure),
      }
      forget(pager, id);
      Ok(())
    });
  };
  let email = invitation.email.clone();
  let confirm_email = invitation.email.clone();
  let sponsor = invitation.sponsor.display_name.clone();
  let maybe_expires_at = invitation.maybe_expires_at;
  let can_resend = invitation.can_resend;
  let is_guest = invitation.role == SpaceRole::Guest;
  let is_expired = invitation.is_expired;
  view! {
    <li class="page-row directory-invitation">
      <span class="page-portrait-group"><Icon name=IconName::UserPlus /></span>
      <span class="page-row-text directory-row-text">
        <span class="page-row-title" translate="no">{email}</span>
        <span class="page-row-detail">
          <span translate="no">{invitation.space_name.clone()}</span>
          " · "
          {move || i18n.t_with("directory-invited-by", &[("name", &sponsor)])}
        </span>
        {maybe_expires_at.filter(|_| !is_expired).map(|at| view! {
          <span class="page-row-detail">
            {move || i18n.t_with("directory-invitation-expires", &[("date", &format_date(at.0, i18n.locale().tag()))])}
          </span>
        })}
      </span>
      {(is_guest && state.shows_guest_markers()).then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
      {is_expired.then(|| view! { <span class="badge">{move || i18n.t("directory-invitation-expired")}</span> })}
      <span class="directory-invitation-actions">
        <Show
          when=move || !is_confirming.get()
          fallback={
            let confirm_email = confirm_email.clone();
            move || view! {
              <span role="alert" class="muted-text">{i18n.t_with("directory-revoke-confirm", &[("email", &confirm_email)])}</span>
              <PendingButton pending=revoke class="btn btn-small btn-danger" on_click=on_revoke>
                {move || i18n.t("directory-revoke")}
              </PendingButton>
              <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
            }
          }
        >
          {can_resend.then(|| view! {
            <Show
              when=move || !is_resent.get()
              fallback=move || view! { <span class="muted-text" role="status">{move || i18n.t("directory-resent")}</span> }
            >
              <PendingButton pending=resend class="btn btn-small" on_click=on_resend>
                {move || i18n.t("directory-resend")}
              </PendingButton>
            </Show>
          })}
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(true)>{move || i18n.t("directory-revoke")}</button>
        </Show>
      </span>
    </li>
  }
}

async fn call(
  maybe_client: Option<RestClient>,
  method: ApiMethod,
  path: String,
) -> Result<(), ActionFailure> {
  let Some(client) = maybe_client else {
    return Err(ActionFailure::new(FailureKind::Other, String::new()));
  };
  client
    .send(method, &path, None)
    .await
    .map_err(|refusal| refusal.to_failure())
}

fn is_gone(failure: &ActionFailure) -> bool {
  failure.kind == FailureKind::Server(ErrorCode::InviteNotFound)
}

fn forget(pager: Pager<DirectoryInvitation>, id: InviteId) {
  pager.remove(|invitation| invitation.id == id);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_a_missing_invitation_counts_as_gone() {
    let refusal = |code| ActionFailure::new(FailureKind::Server(code), String::new());
    assert!(is_gone(&refusal(ErrorCode::InviteNotFound)));
    assert!(!is_gone(&refusal(ErrorCode::Conflict)));
    assert!(!is_gone(&ActionFailure::new(FailureKind::Network, String::new())));
  }
}
