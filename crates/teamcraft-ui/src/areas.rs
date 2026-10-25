//! Areas of the app in wasm chunks of their own (#334): the code an area alone uses (the
//! administration console, …) is split off the app and loads the first time the area opens, so the
//! first visit downloads less (robustness.md, "Download size"; docs/performance.md, "Web bundle
//! size"). Code several areas share goes into shared chunks; what the first screen needs stays in
//! the app.
//!
//! The signed-in app itself is one (`session`): a guest who opens a meeting link loads only the
//! guest pages, and `init.js` fetches the session's chunks next to the app for every other page, so
//! signed-in users don't wait for them one after the other.
//!
//! Each split point is a `wasm_split` function that builds the area's view; [`lazy_area`] calls it,
//! shows a placeholder while the chunk loads and, when it can't load (offline, a deploy in between),
//! says so with a retry instead of an empty view. The split itself is done by
//! `teamcraft-web-bundle` after the build; on other targets (tests, desktop shell tools) the
//! functions are called directly.

use std::future::Future;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local_scoped;
use leptos::wasm_split::{SplitLoaderError, wasm_split};

use crate::admin::AdminSection;
use crate::calls::CallRequest;
use crate::components::admin_console::AdminConsole;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::directory_view::DirectoryView;
use crate::components::lightbox::Lightbox;
use crate::components::login::SessionGate;
use crate::components::meetings_view::{MeetingsView, ScheduleMeetingDialog};
use crate::components::search_view::SearchView;
use crate::components::settings_modal::SettingsModal;
use crate::components::space_invites::{AddToChannelDialog, BrowseSpacesDialog, InvitePeopleDialog};
use crate::components::workspace_dialogs::{
  BrowseChannelsDialog, ChannelSettingsDialog, NewChannelDialog, NewSpaceDialog, SpaceSettingsDialog,
};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::model::{ConversationId, SpaceId};
use crate::reactive::SetIfAlive;
use crate::settings::Settings;
use crate::state::AppState;

/// Where an area shows: how its placeholder and its error sit in the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
  /// The main pane: the placeholder fills it.
  Pane,
  /// A dialog or a screen above the app: the placeholder floats, the error is a dialog that closes
  /// the overlay.
  Overlay,
  /// The whole window (the signed-in app, the call screens).
  Screen,
}

/// Where an area stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
  Loading,
  Ready,
  Failed,
}

/// Declares split points: `name(args) => module: view` builds `view` in chunk `module`.
macro_rules! areas {
  ($( $(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) => $module:ident: $view:expr; )*) => {
    $(
      $(#[$doc])*
      #[wasm_split($module, wasm_split_path = ::leptos::wasm_split, fallible)]
      pub fn $name($($arg: $ty),*) -> Result<AnyView, SplitLoaderError> {
        Ok($view.into_any())
      }
    )*
  };
}

areas! {
  /// Sign-in and the signed-in app: everything but the guest pages.
  session(settings: Settings) => session: view! { <SessionGate settings=settings /> };
  /// The administration console: the largest part of the app, for admins only.
  admin_console(section: AdminSection) => admin: view! { <AdminConsole section=section /> };
  settings(_unit: ()) => settings: view! { <SettingsModal /> };
  lightbox(conversation: ConversationId, start: MediaKey) => lightbox: view! { <Lightbox conversation=conversation start=start /> };
  /// Creating and configuring spaces and channels, and inviting people.
  new_space(_unit: ()) => workspace: view! { <NewSpaceDialog /> };
  new_channel(space: SpaceId) => workspace: view! { <NewChannelDialog space=space /> };
  browse_channels(space: SpaceId) => workspace: view! { <BrowseChannelsDialog space=space /> };
  channel_settings(conversation: ConversationId) => workspace: view! { <ChannelSettingsDialog conversation=conversation /> };
  space_settings(space: SpaceId) => workspace: view! { <SpaceSettingsDialog space=space /> };
  browse_spaces(maybe_highlight: Option<SpaceId>) => workspace: view! { <BrowseSpacesDialog maybe_highlight=maybe_highlight /> };
  invite_people(space: SpaceId) => workspace: view! { <InvitePeopleDialog space=space /> };
  add_to_channel(conversation: ConversationId) => workspace: view! { <AddToChannelDialog conversation=conversation /> };
  /// The call screens (the incoming call card stays in the app, so a call rings at once).
  call_prejoin(request: CallRequest) => calls: view! { <PreJoinScreen request=request /> };
  call_screen(request: CallRequest) => calls: view! { <CallScreen request=request /> };
  meetings(_unit: ()) => meetings: view! { <MeetingsView /> };
  schedule_meeting(_unit: ()) => meetings: view! { <ScheduleMeetingDialog /> };
  directory(_unit: ()) => directory: view! { <DirectoryView /> };
  search(_unit: ()) => search: view! { <SearchView /> };
}

/// Shows the view `load` builds once its chunk is there: a placeholder until then, and a message
/// with a retry when the chunk can't be loaded.
pub fn lazy_area<F, Fut>(place: Place, load: F) -> AnyView
where
  F: Fn() -> Fut + 'static,
  Fut: Future<Output = Result<AnyView, SplitLoaderError>> + 'static,
{
  let i18n = use_i18n();
  let phase = RwSignal::new(Phase::Loading);
  let ready: StoredValue<Option<AnyView>, LocalStorage> = StoredValue::new_local(None);
  let start: Rc<dyn Fn()> = Rc::new(move || {
    phase.set(Phase::Loading);
    let loading = load();
    // Scoped: the area's view belongs to this place in the page and goes when it goes.
    spawn_local_scoped(async move {
      match loading.await {
        Ok(view) => {
          // The area may be gone by now; then the view goes with the task.
          let _ = ready.try_update_value(|slot| *slot = Some(view));
          phase.set_if_alive(Phase::Ready);
        },
        Err(error) => {
          tracing::warn!(%error, "a part of the app did not load");
          phase.set_if_alive(Phase::Failed);
        },
      }
    });
  });
  start();
  // Views are `Send`; the loader stays on this thread.
  let start = StoredValue::new_local(start);
  let class = place.class();
  view! {
    {move || match phase.get() {
      Phase::Loading => {
        view! {
          <div class=format!("lazy-area {class}") role="status" aria-busy="true">
            <span class="spinner" aria-hidden="true"></span>
          </div>
        }
        .into_any()
      },
      Phase::Ready => ready
        .try_update_value(Option::take)
        .flatten()
        .unwrap_or_else(|| ().into_any()),
      Phase::Failed => {
        let retry = move |_| start.with_value(|start| start());
        let maybe_close = (place == Place::Overlay).then(|| {
          let maybe_state = use_context::<AppState>();
          view! {
            <button class="btn" on:click=move |_| maybe_state.iter().for_each(AppState::close_overlay)>
              {move || i18n.t("close")}
            </button>
          }
        });
        view! {
          <div class=format!("lazy-area lazy-area-failed {class}") role="alert">
            <div class="lazy-area-card">
              <p>{move || i18n.t("area-load-failed")}</p>
              <div class="lazy-area-actions">
                {maybe_close}
                <button class="btn btn-primary" on:click=retry>{move || i18n.t("loader-retry")}</button>
              </div>
            </div>
          </div>
        }
        .into_any()
      },
    }}
  }
  .into_any()
}

impl Place {
  fn class(self) -> &'static str {
    match self {
      Self::Pane => "lazy-area-pane",
      Self::Overlay => "lazy-area-overlay",
      Self::Screen => "lazy-area-screen",
    }
  }
}
