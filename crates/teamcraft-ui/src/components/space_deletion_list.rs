//! The channels a space's deletion takes, in its confirmation (e2ee.md §4.6.5, D7, issue #1727):
//! each with who last set its name, as the channel's anchor on this device says. A name a member
//! set doesn't let the deletion purge at once, so the list says so; members' devices keep that
//! channel's history for a while. The device signs exactly the channels listed here.

use leptos::prelude::*;
use teamcraft_client_api::model::{RenamerRole, SpaceDeletionPreviewChannel};

use crate::i18n::use_i18n;
use crate::model::SpaceId;
use crate::state::use_app_state;

/// Asks the core to list the space's channels when shown, and lists them once it did.
#[component]
pub fn SpaceDeletionList(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  state.try_write(|backend| backend.prepare_space_deletion(space));
  let preview = Signal::derive(move || state.read(|backend| backend.space_deletion_preview(space)));
  view! {
    {move || match preview.get() {
      None => view! { <p class="hint" aria-live="polite">{move || i18n.t("space-delete-listing")}</p> }.into_any(),
      Some(preview) if preview.channels.is_empty() => ().into_any(),
      Some(preview) => {
        let is_signed = preview.is_signed;
        view! {
          <ul class="space-deletion-list" aria-label=move || i18n.t("space-delete-channels")>
            {preview
              .channels
              .into_iter()
              .map(|channel| view! { <SpaceDeletionRow channel=channel is_signed=is_signed /> })
              .collect_view()}
          </ul>
        }
        .into_any()
      },
    }}
  }
}

#[component]
fn SpaceDeletionRow(channel: SpaceDeletionPreviewChannel, is_signed: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let renamer = channel.maybe_renamed_by.map(|renamer| {
    let name = state
      .read(|backend| backend.user(renamer.user))
      .map(|user| user.display_name)
      .unwrap_or_default();
    (name, renamer.role)
  });
  view! {
    <li>
      <span class="space-deletion-name" translate="no">{format!("#{}", channel.name)}</span>
      {renamer.map(|(name, role)| {
        let key = match role {
          RenamerRole::Founder => "space-delete-named-by-founder",
          RenamerRole::ChannelAdmin | RenamerRole::WorkspaceAdmin => "space-delete-renamed-by-admin",
          RenamerRole::Member => "space-delete-renamed-by-member",
        };
        let is_kept = is_signed && role == RenamerRole::Member;
        view! {
          <span class="hint">{move || i18n.t_with(key, &[("name", &name)])}</span>
          {is_kept.then(|| view! { <span class="hint">{move || i18n.t("space-delete-member-name-kept")}</span> })}
        }
      })}
    </li>
  }
}
