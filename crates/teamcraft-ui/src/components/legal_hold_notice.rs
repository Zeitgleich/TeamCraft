//! The legal hold banner (issue #211, data-export.md §4.2): every member of a held conversation
//! reads that it is held, with the case reference, and that retention doesn't delete its messages.
//! The hold comes from the verified, owner-signed policy only, so the server can neither show it
//! for a conversation nobody held nor hide it.

use leptos::prelude::*;
use teamcraft_proto::ConversationId;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

#[component]
pub fn LegalHoldNotice(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hold = Memo::new(move |_| state.read(|backend| backend.legal_hold(conversation)));
  move || {
    hold.get().map(|hold| {
      view! {
        <div class="key-banner legal-hold-notice" role="status">
          <Icon name=IconName::Shield />
          <div class="key-banner-text">
            <span>{move || i18n.t_with("legal-hold-notice", &[("reference", &hold.reference)])}</span>
          </div>
        </div>
      }
    })
  }
}
